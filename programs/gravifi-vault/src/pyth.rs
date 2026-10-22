//! Pyth prices, read by hand from a `PriceUpdateV2` account.
//!
//! The pyth-solana-receiver-sdk crate would pull in Wormhole types this program never uses, so
//! the one account it reads is decoded here. Layout (Pyth receiver, Anchor account):
//! discriminator (8), write_authority (32), verification_level (Borsh enum: 0 = Partial
//! { num_signatures: u8 }, 1 = Full), then the price message: feed_id (32), price (i64),
//! conf (u64), exponent (i32), publish_time (i64), prev_publish_time, ema_price, ema_conf,
//! and posted_slot. Anyone can post an update, but only through the receiver, which verifies it
//! against Wormhole's guardians; Full means all of them signed.
use anchor_lang::prelude::*;

use crate::errors::VaultError;

pub const RECEIVER_ID: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
/// sha256("account:PriceUpdateV2")[..8]
const DISCRIMINATOR: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];
const FULL: u8 = 1;

pub struct PythPrice {
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    pub publish_time: i64,
}

impl PythPrice {
    /// USD x1e12 per whole token, for a token whose wallet amounts are its raw amount times
    /// `multiplier` (a scaled UI amount; 1 otherwise): Pyth quotes the token as wallets show it.
    pub fn e12(&self, value: u64, multiplier: f64) -> Option<u128> {
        let shift = 12 + self.exponent;
        let scaled = if shift >= 0 {
            (value as u128).checked_mul(10u128.checked_pow(shift as u32)?)?
        } else {
            (value as u128) / 10u128.checked_pow((-shift) as u32)?
        };
        let out = scaled as f64 * multiplier;
        (out.is_finite() && out >= 0.0 && out < u128::MAX as f64).then_some(out as u128)
    }
}

/// The price in a `PriceUpdateV2` account for `feed_id`: fully verified, and no older than
/// `max_age` seconds at `now`.
pub fn read(ai: &AccountInfo, feed_id: &[u8; 32], now: i64, max_age: i64) -> Result<PythPrice> {
    require_keys_eq!(*ai.owner, RECEIVER_ID, VaultError::PythPriceInvalid);
    let d = ai.try_borrow_data()?;
    require!(d.len() >= 8 + 32 + 1 + 32 + 8 + 8 + 4 + 8 && d[..8] == DISCRIMINATOR, VaultError::PythPriceInvalid);
    require!(d[40] == FULL, VaultError::PythPriceInvalid);
    let m = &d[41..];
    require!(&m[..32] == feed_id, VaultError::PythPriceInvalid);
    let i64_at = |o: usize| i64::from_le_bytes(m[o..o + 8].try_into().unwrap());
    let p = PythPrice {
        price: i64_at(32),
        conf: u64::from_le_bytes(m[40..48].try_into().unwrap()),
        exponent: i32::from_le_bytes(m[48..52].try_into().unwrap()),
        publish_time: i64_at(52),
    };
    require!(p.price > 0, VaultError::PythPriceInvalid);
    require!(now - p.publish_time <= max_age, VaultError::PythPriceStale);
    Ok(p)
}
