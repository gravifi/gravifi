//! The share token's name, ticker and logo, as wallets show them: its Metaplex metadata. The
//! basket PDA is the mint authority and the metadata's update authority, so only this
//! instruction can ever write it. It changes nothing a holder owns, so an immutable basket may
//! change it too, as it may change its image.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;

use crate::errors::VaultError;
use crate::metaplex;
use crate::state::*;

/// Creates the share's metadata the first time, and replaces its name, symbol and URI after.
/// The basket's creator or the protocol authority may call it; `payer` pays Metaplex's fee and
/// the rent the first time (the creator on mainnet, the server on the testnet).
pub fn set_share_metadata(ctx: Context<SetShareMetadata>, name: String, symbol: String, uri: String) -> Result<()> {
    let a = &ctx.accounts;
    let signer = a.authority.key();
    require!(signer == a.basket.creator || signer == a.protocol.authority, VaultError::NotMetadataAuthority);
    require!(
        (1..=metaplex::MAX_NAME_LEN).contains(&name.len())
            && (1..=metaplex::MAX_SYMBOL_LEN).contains(&symbol.len())
            && (1..=metaplex::MAX_URI_LEN).contains(&uri.len()),
        VaultError::BadMetadata
    );
    // Metaplex checks the program it's handed against the mint's owner.
    require_keys_eq!(*a.share_mint.owner, a.token_program.key(), VaultError::BadParameter);

    let accounts = metaplex::MetadataAccounts {
        metadata: a.metadata.key(),
        mint: a.share_mint.key(),
        authority: a.basket.key(),
        payer: a.payer.key(),
        token_program: a.token_program.key(),
    };
    let ix = if a.metadata.data_is_empty() {
        metaplex::create(&accounts, &name, &symbol, &uri)
    } else {
        metaplex::update(&accounts, &name, &symbol, &uri)
    };
    let basket = &a.basket;
    let bump = [basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", basket.creator.as_ref(), basket.basket_id.as_ref(), &bump];
    invoke_signed(
        &ix,
        &[
            a.metadata.to_account_info(),
            a.share_mint.to_account_info(),
            a.basket.to_account_info(),
            a.payer.to_account_info(),
            a.system_program.to_account_info(),
            a.sysvar_instructions.to_account_info(),
            a.token_program.to_account_info(),
            a.token_metadata_program.to_account_info(),
        ],
        &[&seeds],
    )?;
    Ok(())
}

#[derive(Accounts)]
pub struct SetShareMetadata<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    /// CHECK: the basket's share mint (has_one above); Metaplex reads it.
    #[account(mut)]
    pub share_mint: UncheckedAccount<'info>,
    /// CHECK: the share mint's metadata account, at Metaplex's address for it.
    #[account(
        mut,
        seeds = [b"metadata", metaplex::TOKEN_METADATA_ID.as_ref(), share_mint.key().as_ref()],
        bump,
        seeds::program = metaplex::TOKEN_METADATA_ID
    )]
    pub metadata: UncheckedAccount<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    /// CHECK: the share mint's token program, matched against the mint's owner.
    pub token_program: UncheckedAccount<'info>,
    /// CHECK: Metaplex Token Metadata.
    #[account(address = metaplex::TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: the instructions sysvar, which Metaplex reads.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub sysvar_instructions: UncheckedAccount<'info>,
}
