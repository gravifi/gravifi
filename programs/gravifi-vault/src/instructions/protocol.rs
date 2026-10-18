//! Protocol administration: the protocol's settings and fees, which assets are listed, who
//! the keepers are, oracle prices, and the treasury.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;

/// One-time protocol setup, gated on the program's upgrade authority. The Protocol
/// account is a singleton, so without that gate anyone could initialise it first and
/// appoint themselves the authority that decides which assets exist.
pub fn init_protocol(
    ctx: Context<InitProtocol>,
    gravi_mint: Pubkey,
    protocol_fee_bps: u16,
    burn_share_bps: u16,
) -> Result<()> {
    assert_upgrade_authority(
        &ctx.accounts.program_data.to_account_info(),
        &ctx.accounts.authority.key(),
    )?;
    require!(protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS, VaultError::FeeTooHigh);
    check_burn_share(burn_share_bps)?;

    let p = &mut ctx.accounts.protocol;
    p.bump = ctx.bumps.protocol;
    p.authority = ctx.accounts.authority.key();
    p.guardian = Pubkey::default();
    p.gravi_mint = gravi_mint;
    p.protocol_fee_bps = protocol_fee_bps;
    p.burn_share_bps = burn_share_bps;
    p.listed_count = 0;
    Ok(())
}

/// Hands the protocol to a new authority, e.g. from the deployer to the team multisig.
pub fn set_protocol_authority(ctx: Context<ProtocolAdmin>, new_authority: Pubkey) -> Result<()> {
    ctx.accounts.protocol.authority = new_authority;
    Ok(())
}

/// Sets the GRAVI mint, once. The token launches after the program, so `init_protocol`
/// may leave it unset; after this it can never change.
pub fn set_gravi_mint(ctx: Context<ProtocolAdmin>, gravi_mint: Pubkey) -> Result<()> {
    let p = &mut ctx.accounts.protocol;
    require!(p.gravi_mint == Pubkey::default(), VaultError::GraviMintAlreadySet);
    require!(gravi_mint != Pubkey::default(), VaultError::BadParameter);
    p.gravi_mint = gravi_mint;
    Ok(())
}

/// `Pubkey::default()` removes the guardian.
pub fn set_guardian(ctx: Context<ProtocolAdmin>, guardian: Pubkey) -> Result<()> {
    ctx.accounts.protocol.guardian = guardian;
    Ok(())
}

pub fn set_protocol_fees(
    ctx: Context<ProtocolAdmin>,
    protocol_fee_bps: u16,
    burn_share_bps: u16,
) -> Result<()> {
    require!(protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS, VaultError::FeeTooHigh);
    check_burn_share(burn_share_bps)?;
    let p = &mut ctx.accounts.protocol;
    p.protocol_fee_bps = protocol_fee_bps;
    p.burn_share_bps = burn_share_bps;
    Ok(())
}

/// Adds a mint to the set of assets baskets may be built from.
pub fn register_asset(ctx: Context<RegisterAsset>) -> Result<()> {
    assert_no_active_transfer_hook(&ctx.accounts.mint.to_account_info())?;
    TransferFee::of(&ctx.accounts.mint.to_account_info())?.assert_within_cap()?;

    let e = &mut ctx.accounts.asset_entry;
    e.bump = ctx.bumps.asset_entry;
    e.mint = ctx.accounts.mint.key();
    e.status = AssetStatus::Listed;
    e.listed_at = Clock::get()?.unix_timestamp;
    e.delisted_at = 0;

    let p = &mut ctx.accounts.protocol;
    p.listed_count = p.listed_count.checked_add(1).ok_or(VaultError::MathOverflow)?;
    Ok(())
}

/// Relists or delists a registered asset.
///
/// Delisting pauses minting into the baskets that hold the asset in the same instruction: the
/// remaining accounts are those baskets, each writable. Pausing them here, not in a later
/// `sync_listing_status`, leaves no window in which new money can still go into a basket the
/// protocol has stopped vouching for. (A basket left out can still be paused by that sync.)
/// Relisting unpauses nothing: another of a basket's assets may still be delisted, so that's
/// left to `sync_listing_status`, which checks them all.
///
/// The guardian may delist but never list, and the asymmetry is the point. Listing is
/// the action worth bribing a vote for, so it must go through governance. Delisting is
/// the action that has to happen in minutes when a listed token turns out to be
/// compromised, so it cannot wait out a vote and a timelock.
pub fn set_asset_status<'info>(ctx: Context<'_, '_, 'info, 'info, SetAssetStatus<'info>>, status: AssetStatus) -> Result<()> {
    let signer = ctx.accounts.signer.key();
    let p = &ctx.accounts.protocol;
    let is_authority = signer == p.authority;
    let is_guardian = p.guardian != Pubkey::default() && signer == p.guardian;

    match status {
        AssetStatus::Listed => {
            require!(is_authority, VaultError::NotProtocolAuthority);
            assert_no_active_transfer_hook(&ctx.accounts.mint.to_account_info())?;
        }
        AssetStatus::Delisted => {
            require!(is_authority || is_guardian, VaultError::NotProtocolAuthority);
        }
    }

    // Before the early return below, so a repeat delisting can still pause baskets missed.
    if status == AssetStatus::Delisted {
        let mint = ctx.accounts.mint.key();
        for ai in ctx.remaining_accounts {
            let mut basket = Account::<Basket>::try_from(ai)?;
            require!(basket.active().iter().any(|c| c.mint == mint), VaultError::ConstituentMismatch);
            basket.mint_paused = true;
            basket.exit(&crate::ID)?;
        }
    }

    let now = Clock::get()?.unix_timestamp;
    let e = &mut ctx.accounts.asset_entry;
    if e.status == status {
        return Ok(());
    }
    e.status = status;
    let p = &mut ctx.accounts.protocol;
    match status {
        AssetStatus::Listed => {
            e.listed_at = now;
            p.listed_count = p.listed_count.checked_add(1).ok_or(VaultError::MathOverflow)?;
        }
        AssetStatus::Delisted => {
            e.delisted_at = now;
            p.listed_count = p.listed_count.saturating_sub(1);
        }
    }
    Ok(())
}

/// Moves protocol-treasury shares. Burn-earmarked shares have no equivalent: the burn
/// PDA only signs inside the buyback instructions, so they can only ever be redeemed
/// into the burn PDA's own accounts and sold for GRAVI that is burned.
pub fn treasury_withdraw(ctx: Context<TreasuryWithdraw>, amount: u64) -> Result<()> {
    let bump = [ctx.accounts.protocol.bump];
    let seeds: [&[u8]; 2] = [PROTOCOL_SEED, &bump];
    let signer: &[&[&[u8]]] = &[&seeds];

    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.share_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: ctx.accounts.treasury_shares.to_account_info(),
                mint: ctx.accounts.share_mint.to_account_info(),
                to: ctx.accounts.destination.to_account_info(),
                authority: ctx.accounts.protocol.to_account_info(),
            },
            signer,
        ),
        amount,
        ctx.accounts.share_mint.decimals,
    )
}

/// Names who may post prices and who may run rebalances. Protocol authority only.
pub fn init_keepers(ctx: Context<InitKeepers>, price_authority: Pubkey, rebalancer: Pubkey) -> Result<()> {
    let k = &mut ctx.accounts.keepers;
    k.bump = ctx.bumps.keepers;
    k.price_authority = price_authority;
    k.rebalancer = rebalancer;
    Ok(())
}

pub fn set_keepers(ctx: Context<SetKeepers>, price_authority: Pubkey, rebalancer: Pubkey) -> Result<()> {
    let k = &mut ctx.accounts.keepers;
    k.price_authority = price_authority;
    k.rebalancer = rebalancer;
    Ok(())
}

/// Posts an asset's USD price (per whole token, x1e12). Testnet oracle; on mainnet the
/// rebalancer reads Pyth instead.
pub fn set_price(ctx: Context<SetPrice>, price_e12: u64) -> Result<()> {
    require!(price_e12 > 0, VaultError::BadPrice);
    let decimals = mint_decimals(&ctx.accounts.mint.to_account_info())?;
    let p = &mut ctx.accounts.asset_price;
    p.bump = ctx.bumps.asset_price;
    p.mint = ctx.accounts.mint.key();
    p.decimals = decimals;
    p.price_e12 = price_e12;
    p.updated_at = Clock::get()?.unix_timestamp;
    Ok(())
}

/// Checks `expected` is the upgrade authority recorded in this program's ProgramData.
///
/// Parsed by hand rather than through `Account<ProgramData>`, which drags a bincode
/// deserialiser into the binary — tens of KB of rent to read one field. The layout is the
/// loader's fixed bincode encoding of `UpgradeableLoaderState::ProgramData`:
/// `u32` variant (3), `u64` slot, `u8` option tag, then the 32-byte authority.
/// The burn's share of the protocol fee: anywhere from `MIN_BURN_SHARE_BPS` (half) to all of it.
/// The floor is code, like the fee ceilings, so the half that buys GRAVI and burns it can't be
/// turned down by the authority's key.
pub(crate) fn check_burn_share(burn_share_bps: u16) -> Result<()> {
    require!(
        burn_share_bps >= MIN_BURN_SHARE_BPS && burn_share_bps as u128 <= BPS_DENOM,
        VaultError::BurnShareTooLow
    );
    Ok(())
}

pub(crate) fn assert_upgrade_authority(program_data: &AccountInfo, expected: &Pubkey) -> Result<()> {
    require_keys_eq!(
        *program_data.owner,
        anchor_lang::solana_program::bpf_loader_upgradeable::ID,
        VaultError::NotProtocolAuthority
    );
    let data = program_data.try_borrow_data()?;
    require!(data.len() >= 45, VaultError::NotProtocolAuthority);
    require!(data[0..4] == 3u32.to_le_bytes(), VaultError::NotProtocolAuthority);
    require!(data[12] == 1, VaultError::NotProtocolAuthority);
    require!(&data[13..45] == expected.as_ref(), VaultError::NotProtocolAuthority);
    Ok(())
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct InitProtocol<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + Protocol::INIT_SPACE,
        seeds = [PROTOCOL_SEED],
        bump
    )]
    pub protocol: Account<'info, Protocol>,

    /// CHECK: must be this program's ProgramData account; the upgrade authority is read
    /// from it by hand in `assert_upgrade_authority`.
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = anchor_lang::solana_program::bpf_loader_upgradeable::ID
    )]
    pub program_data: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ProtocolAdmin<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol.bump,
        has_one = authority @ VaultError::NotProtocolAuthority
    )]
    pub protocol: Account<'info, Protocol>,
}

#[derive(Accounts)]
pub struct RegisterAsset<'info> {
    /// Pays the entry's rent. Separate from `authority`, which is usually a multisig.
    #[account(mut)]
    pub payer: Signer<'info>,

    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol.bump,
        has_one = authority @ VaultError::NotProtocolAuthority
    )]
    pub protocol: Account<'info, Protocol>,

    pub mint: InterfaceAccount<'info, MintI>,

    #[account(
        init,
        payer = payer,
        space = 8 + AssetEntry::INIT_SPACE,
        seeds = [ASSET_SEED, mint.key().as_ref()],
        bump
    )]
    pub asset_entry: Account<'info, AssetEntry>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetAssetStatus<'info> {
    /// Authority or guardian; which one is allowed depends on the requested status.
    pub signer: Signer<'info>,

    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,

    pub mint: InterfaceAccount<'info, MintI>,

    #[account(
        mut,
        seeds = [ASSET_SEED, mint.key().as_ref()],
        bump = asset_entry.bump
    )]
    pub asset_entry: Account<'info, AssetEntry>,
}

#[derive(Accounts)]
pub struct TreasuryWithdraw<'info> {
    pub authority: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol.bump,
        has_one = authority @ VaultError::NotProtocolAuthority
    )]
    pub protocol: Account<'info, Protocol>,

    pub share_mint: InterfaceAccount<'info, MintI>,

    #[account(mut, token::mint = share_mint, token::authority = protocol)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    #[account(mut, token::mint = share_mint)]
    pub destination: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct InitKeepers<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    #[account(init, payer = authority, space = 8 + Keepers::INIT_SPACE, seeds = [KEEPERS_SEED], bump)]
    pub keepers: Account<'info, Keepers>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetKeepers<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    #[account(mut, seeds = [KEEPERS_SEED], bump = keepers.bump)]
    pub keepers: Account<'info, Keepers>,
}

#[derive(Accounts)]
pub struct SetPrice<'info> {
    #[account(mut)]
    pub price_authority: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.price_authority == price_authority.key() @ VaultError::NotPriceAuthority)]
    pub keepers: Account<'info, Keepers>,
    pub mint: InterfaceAccount<'info, MintI>,
    #[account(init_if_needed, payer = price_authority, space = 8 + AssetPrice::INIT_SPACE, seeds = [PRICE_SEED, mint.key().as_ref()], bump)]
    pub asset_price: Account<'info, AssetPrice>,
    pub system_program: Program<'info, System>,
}
