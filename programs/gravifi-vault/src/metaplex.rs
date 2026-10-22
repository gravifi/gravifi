//! Metaplex Token Metadata's Create and Update, hand-encoded.
//!
//! Wallets name a token, and show its logo, from its Metaplex metadata account. The
//! mpl-token-metadata crate would pull in far more than these two instructions, so they are
//! written out here. Layouts follow mpl-token-metadata 5.x: instruction 42 (Create), variant 0
//! = V1 { asset_data, decimals: None, print_supply: None }; instruction 50 (Update), variant 0
//! = V1 with only `data` set. The older CreateMetadataAccountV3 is disabled on mainnet.
//!
//! Creating charges Metaplex's protocol fee (0.01 SOL, kept in the metadata account) on top of
//! its rent; updating charges nothing.
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};

pub const TOKEN_METADATA_ID: Pubkey = pubkey!("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const CREATE: u8 = 42;
const UPDATE: u8 = 50;
const FUNGIBLE: u8 = 2;
/// Metaplex's own limits, in bytes.
pub const MAX_NAME_LEN: usize = 32;
pub const MAX_SYMBOL_LEN: usize = 10;
pub const MAX_URI_LEN: usize = 200;

/// The share's display data: no creators and no royalties, since shares are fungible.
fn data(out: &mut Vec<u8>, name: &str, symbol: &str, uri: &str) {
    for s in [name, symbol, uri] {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes()); // seller_fee_basis_points
    out.push(0); // creators: None
}

pub struct MetadataAccounts {
    pub metadata: Pubkey,
    pub mint: Pubkey,
    /// Mint authority and update authority both: the basket PDA.
    pub authority: Pubkey,
    pub payer: Pubkey,
    pub token_program: Pubkey,
}

pub fn create(a: &MetadataAccounts, name: &str, symbol: &str, uri: &str) -> Instruction {
    let mut d = vec![CREATE, 0];
    data(&mut d, name, symbol, uri);
    d.push(0); // primary_sale_happened
    d.push(1); // is_mutable
    d.push(FUNGIBLE);
    d.extend_from_slice(&[0, 0, 0, 0]); // collection, uses, collection_details, rule_set: None
    d.extend_from_slice(&[0, 0]); // decimals (read from the mint), print_supply: None
    Instruction {
        program_id: TOKEN_METADATA_ID,
        accounts: vec![
            AccountMeta::new(a.metadata, false),
            AccountMeta::new_readonly(TOKEN_METADATA_ID, false), // master edition: none
            AccountMeta::new(a.mint, false),
            AccountMeta::new_readonly(a.authority, true),
            AccountMeta::new(a.payer, true),
            AccountMeta::new_readonly(a.authority, true),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(anchor_lang::solana_program::sysvar::instructions::ID, false),
            AccountMeta::new_readonly(a.token_program, false),
        ],
        data: d,
    }
}

pub fn update(a: &MetadataAccounts, name: &str, symbol: &str, uri: &str) -> Instruction {
    let mut d = vec![UPDATE, 0];
    d.push(0); // new_update_authority: None
    d.push(1); // data: Some
    data(&mut d, name, symbol, uri);
    d.extend_from_slice(&[0, 0]); // primary_sale_happened, is_mutable: None
    d.extend_from_slice(&[0, 0, 0, 0]); // collection, collection_details, uses, rule_set: unchanged
    d.push(0); // authorization_data: None
    let none = AccountMeta::new_readonly(TOKEN_METADATA_ID, false);
    Instruction {
        program_id: TOKEN_METADATA_ID,
        accounts: vec![
            AccountMeta::new_readonly(a.authority, true),
            none.clone(), // delegate record
            none.clone(), // token
            AccountMeta::new_readonly(a.mint, false),
            AccountMeta::new(a.metadata, false),
            none.clone(), // edition
            AccountMeta::new(a.payer, true),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(anchor_lang::solana_program::sysvar::instructions::ID, false),
            none.clone(), // authorization rules program
            none,         // authorization rules
        ],
        data: d,
    }
}
