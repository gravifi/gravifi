//! A basket's extension account, at `[EXT_SEED, basket]`: room for per-basket fields the Basket
//! account has no space for (its asset list is variable-length and last, so it can't grow without
//! migrating every basket). Nothing is created up front: whoever first turns on a feature that
//! needs it creates it and pays its rent (`ensure`), and the feature keeps its fields in a typed
//! record there (`get` / `put`).
//!
//! Layout: [discriminator 8][version 1][bump 1][basket 32][used u16][records EXT_RECORDS_LEN], each
//! record `[tag u16][len u16][bytes]`. A tag names one feature's record, for good; a record keeps
//! its length once written.

use anchor_lang::prelude::*;

use crate::errors::VaultError;

pub const EXT_SEED: &[u8] = b"ext";
/// sha256("account:BasketExt")[..8].
pub const EXT_DISCRIMINATOR: [u8; 8] = [228, 193, 107, 29, 16, 166, 178, 60];
pub const EXT_VERSION: u8 = 1;
pub const EXT_RECORDS_LEN: usize = 256;
const BASKET_AT: usize = 10;
const USED_AT: usize = 42;
const RECORDS_AT: usize = 44;
pub const EXT_LEN: usize = RECORDS_AT + EXT_RECORDS_LEN;

/// The extension account's address and bump for `basket`.
pub fn address(basket: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EXT_SEED, basket.as_ref()], &crate::ID)
}

/// Creates `basket`'s extension account at `ext` if it isn't there yet, `payer` paying its rent.
/// Refuses any other address.
pub fn ensure<'info>(ext: &AccountInfo<'info>, basket: &Pubkey, payer: &AccountInfo<'info>, system_program: &AccountInfo<'info>) -> Result<()> {
    let (expected, bump) = address(basket);
    require_keys_eq!(*ext.key, expected, VaultError::BadExt);
    if ext.owner == &crate::ID {
        return Ok(());
    }
    crate::token_util::create_pda(payer, ext, system_program, &[EXT_SEED, basket.as_ref(), &[bump]], EXT_LEN)?;
    let mut d = ext.try_borrow_mut_data()?;
    d[..8].copy_from_slice(&EXT_DISCRIMINATOR);
    d[8] = EXT_VERSION;
    d[9] = bump;
    d[BASKET_AT..USED_AT].copy_from_slice(basket.as_ref());
    Ok(())
}

/// Checks `data` is an extension account's, for `basket`.
pub fn check(data: &[u8], basket: &Pubkey) -> Result<()> {
    require!(data.len() >= EXT_LEN && data[..8] == EXT_DISCRIMINATOR && &data[BASKET_AT..USED_AT] == basket.as_ref(), VaultError::BadExt);
    Ok(())
}

fn used(data: &[u8]) -> usize {
    u16::from_le_bytes([data[USED_AT], data[USED_AT + 1]]) as usize
}

/// Where record `tag` starts among the records, and its length.
fn find(data: &[u8], tag: u16) -> Option<(usize, usize)> {
    let (records, end) = (&data[RECORDS_AT..], used(data));
    let mut at = 0;
    while at + 4 <= end {
        let t = u16::from_le_bytes([records[at], records[at + 1]]);
        let len = u16::from_le_bytes([records[at + 2], records[at + 3]]) as usize;
        if t == tag {
            return Some((at + 4, len));
        }
        at += 4 + len;
    }
    None
}

/// Record `tag`'s bytes, if written.
pub fn get(data: &[u8], tag: u16) -> Option<&[u8]> {
    find(data, tag).map(|(at, len)| &data[RECORDS_AT + at..RECORDS_AT + at + len])
}

/// Writes record `tag`: in place if it exists (same length), appended if not, while there's room.
pub fn put(data: &mut [u8], tag: u16, bytes: &[u8]) -> Result<()> {
    if let Some((at, len)) = find(data, tag) {
        require!(len == bytes.len(), VaultError::BadExt);
        data[RECORDS_AT + at..RECORDS_AT + at + len].copy_from_slice(bytes);
        return Ok(());
    }
    let end = used(data);
    let next = end + 4 + bytes.len();
    require!(next <= EXT_RECORDS_LEN, VaultError::ExtFull);
    let o = RECORDS_AT + end;
    data[o..o + 2].copy_from_slice(&tag.to_le_bytes());
    data[o + 2..o + 4].copy_from_slice(&(bytes.len() as u16).to_le_bytes());
    data[o + 4..o + 4 + bytes.len()].copy_from_slice(bytes);
    data[USED_AT..USED_AT + 2].copy_from_slice(&(next as u16).to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records() {
        let basket = Pubkey::new_unique();
        let mut d = vec![0u8; EXT_LEN];
        d[..8].copy_from_slice(&EXT_DISCRIMINATOR);
        d[BASKET_AT..USED_AT].copy_from_slice(basket.as_ref());
        check(&d, &basket).unwrap();
        assert!(check(&d, &Pubkey::new_unique()).is_err());
        assert!(get(&d, 1).is_none());
        put(&mut d, 1, &[1, 2, 3]).unwrap();
        put(&mut d, 7, &42u64.to_le_bytes()).unwrap();
        assert_eq!(get(&d, 1), Some(&[1u8, 2, 3][..]));
        put(&mut d, 1, &[9, 9, 9]).unwrap();
        assert_eq!(get(&d, 1), Some(&[9u8, 9, 9][..]));
        assert_eq!(get(&d, 7), Some(&42u64.to_le_bytes()[..]));
        assert!(put(&mut d, 1, &[1]).is_err());
        assert!(put(&mut d, 8, &[0u8; EXT_RECORDS_LEN]).is_err());
    }
}
