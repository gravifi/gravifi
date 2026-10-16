//! Token-2022's Scaled UI Amount extension, hand-encoded.
//!
//! The spl-token-2022 crate this Anchor version pins (v6) predates the extension, so the
//! two instructions and the config layout are written out here. Layouts follow
//! spl-token-2022-interface 2.x: instruction 43 (ScaledUiAmountExtension), sub-instruction
//! 0 = Initialize { authority: Pubkey, multiplier: f64 }, 1 = UpdateMultiplier
//! { multiplier: f64, effective_timestamp: i64 }; extension type 25, 56-byte config.
//!
//! Wallets show a share balance as raw amount x multiplier, so a split only changes the
//! multiplier: nobody's raw balance, and none of the vault's unit math, moves.
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};

pub const SCALED_UI_EXTENSION_TYPE: u16 = 25;
const SCALED_UI_INSTRUCTION: u8 = 43;
/// Base mint padded to the account length (165), one account-type byte, then the TLV entry
/// (2 type + 2 length + 56 config).
pub const MINT_WITH_SCALED_UI_LEN: usize = 165 + 1 + 4 + 56;
pub const PLAIN_MINT_LEN: usize = 82;

pub fn initialize_scaled_ui(token_program: &Pubkey, mint: &Pubkey, authority: &Pubkey, multiplier: f64) -> Instruction {
    let mut data = vec![SCALED_UI_INSTRUCTION, 0];
    data.extend_from_slice(authority.as_ref());
    data.extend_from_slice(&multiplier.to_le_bytes());
    Instruction { program_id: *token_program, accounts: vec![AccountMeta::new(*mint, false)], data }
}

pub fn update_multiplier(token_program: &Pubkey, mint: &Pubkey, authority: &Pubkey, multiplier: f64, effective_timestamp: i64) -> Instruction {
    let mut data = vec![SCALED_UI_INSTRUCTION, 1];
    data.extend_from_slice(&multiplier.to_le_bytes());
    data.extend_from_slice(&effective_timestamp.to_le_bytes());
    Instruction {
        program_id: *token_program,
        accounts: vec![AccountMeta::new(*mint, false), AccountMeta::new_readonly(*authority, true)],
        data,
    }
}

/// The scaled UI config (authority, multiplier, new_multiplier_effective_timestamp,
/// new_multiplier), or None if the mint has no scaled UI amount.
fn scaled_ui_config(mint_data: &[u8]) -> Option<&[u8]> {
    let mut i = 166; // after the padded base mint and the account-type byte
    while i + 4 <= mint_data.len() {
        let ty = u16::from_le_bytes([mint_data[i], mint_data[i + 1]]);
        let len = u16::from_le_bytes([mint_data[i + 2], mint_data[i + 3]]) as usize;
        let value = mint_data.get(i + 4..i + 4 + len)?;
        if ty == SCALED_UI_EXTENSION_TYPE && len >= 56 {
            return Some(value);
        }
        if ty == 0 {
            break;
        }
        i += 4 + len;
    }
    None
}

/// The multiplier in force at `now`, or None if the mint has no scaled UI amount.
pub fn current_multiplier(mint_data: &[u8], now: i64) -> Option<f64> {
    let value = scaled_ui_config(mint_data)?;
    let f = |o: usize| f64::from_le_bytes(value[o..o + 8].try_into().unwrap());
    Some(if now >= last_split_at(mint_data)? { f(48) } else { f(32) })
}

/// When the multiplier last changed. Every split takes effect immediately, so the
/// extension's effective timestamp is the time of the last split (0 before the first).
pub fn last_split_at(mint_data: &[u8]) -> Option<i64> {
    let value = scaled_ui_config(mint_data)?;
    Some(i64::from_le_bytes(value[40..48].try_into().unwrap()))
}
