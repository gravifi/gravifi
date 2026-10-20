//! Permissionless upkeep: streaming the creator fee, and keeping the units promise and the
//! mint pause in line with reality.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;

/// Streams the creator's fee as share dilution. (The protocol takes nothing from
/// holders: its fee is charged on mint and redeem.)
///
/// Minting fee shares without depositing assets would leave `units * supply` above what
/// the vault holds, so units are re-derived from the vault balances and the new supply in
/// the same instruction. The total claim on the assets is unchanged; each share's slice
/// shrinks.
///
/// Re-derived, not scaled: scaling each unit by `old / new` and rounding down lost about
/// a whole unit at every accrual, however small the fee, and for a sleeve worth few
/// units a share (wBTC: ~30,000) that compounded to a vault holding a growing surplus
/// that redemptions never paid out.
///
/// Remaining accounts: each constituent's vault, in basket order.
pub fn accrue_fees<'info>(ctx: Context<'_, '_, 'info, 'info, AccrueFees<'info>>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);

    let elapsed = (now - ctx.accounts.basket.last_fee_accrual).max(0);
    let supply = ctx.accounts.share_mint.supply;
    require!(supply > 0, VaultError::ZeroSupply);

    // At 0% nothing is owed, so the clock moves on: a fee set later starts from then, never
    // from the last accrual before it. (`set_policy` restarts the clock too.)
    if ctx.accounts.basket.mgmt_fee_bps == 0 {
        ctx.accounts.basket.last_fee_accrual = now;
        return Ok(());
    }

    let creator_fee = streamed_fee(supply, ctx.accounts.basket.mgmt_fee_bps, elapsed)?;

    // Otherwise leave the clock alone when nothing was minted. Advancing it anyway would let
    // anyone call this every slot, round each period's fee down to zero, and erase
    // the fee entirely.
    if creator_fee == 0 {
        return Ok(());
    }

    let creator = ctx.accounts.basket.creator;
    let basket_id = ctx.accounts.basket.basket_id;
    let bump = [ctx.accounts.basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let signer: &[&[&[u8]]] = &[&seeds];

    mint_shares(
        &ctx.accounts.share_token_program.to_account_info(),
        &ctx.accounts.share_mint.to_account_info(),
        &ctx.accounts.creator_shares.to_account_info(),
        &ctx.accounts.basket.to_account_info(),
        signer,
        creator_fee,
    )?;

    let new_supply = supply.checked_add(creator_fee).ok_or(VaultError::MathOverflow)?;

    let count = ctx.accounts.basket.constituent_count as usize;
    require!(ctx.remaining_accounts.len() == count, VaultError::BadRemainingAccounts);
    let basket_key = ctx.accounts.basket.key();
    let basket = &mut ctx.accounts.basket;
    for i in 0..count {
        let held = read_vault(&ctx.remaining_accounts[i], &basket_key, &basket.constituents[i].mint)?.amount;
        basket.constituents[i].units_per_share = Basket::live_units(held, new_supply, basket.constituents[i].units_per_share)?;
    }
    basket.last_fee_accrual = now;

    Ok(())
}

/// Permissionless reconciliation of the units promise against what the vault holds.
///
/// Balances move without the vault's participation: a ScaledUiAmount dividend credits
/// the vault (the surplus belongs to holders and should raise units), and a
/// PermanentDelegate clawback debits it (the deficit must lower units, or redemption
/// becomes first-come-first-served). It only ever makes the promise match reality.
pub fn sync_units<'info>(
    ctx: Context<'_, '_, 'info, 'info, SyncUnits<'info>>,
) -> Result<()> {
    let count = ctx.accounts.basket.constituent_count as usize;
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
    require!(
        ctx.remaining_accounts.len() == count,
        VaultError::BadRemainingAccounts
    );

    let supply = ctx.accounts.share_mint.supply;
    require!(supply > 0, VaultError::ZeroSupply);
    let basket_key = ctx.accounts.basket.key();

    for i in 0..count {
        let view = read_vault(&ctx.remaining_accounts[i], &basket_key, &ctx.accounts.basket.constituents[i].mint)?;

        let stored = ctx.accounts.basket.constituents[i].units_per_share;
        ctx.accounts.basket.constituents[i].units_per_share = Basket::live_units(view.amount, supply, stored)?;
    }

    Ok(())
}

/// Permissionless: pauses minting if any constituent is delisted, and unpauses once
/// all of them are listed again. Remaining accounts are the constituents' asset
/// entries, in order.
pub fn sync_listing_status<'info>(
    ctx: Context<'_, '_, 'info, 'info, SyncListing<'info>>,
) -> Result<()> {
    let count = ctx.accounts.basket.constituent_count as usize;
    require!(
        ctx.remaining_accounts.len() == count,
        VaultError::BadRemainingAccounts
    );

    let mut any_delisted = false;
    for i in 0..count {
        let entry = load_asset_entry(&ctx.remaining_accounts[i])?;
        require_keys_eq!(
            entry.mint,
            ctx.accounts.basket.constituents[i].mint,
            VaultError::ConstituentMismatch
        );
        any_delisted |= entry.status == AssetStatus::Delisted;
    }
    ctx.accounts.basket.mint_paused = any_delisted;
    Ok(())
}

pub(crate) fn streamed_fee(supply: u64, bps: u16, elapsed: i64) -> Result<u64> {
    let v = (supply as u128)
        .checked_mul(bps as u128)
        .and_then(|v| v.checked_mul(elapsed.max(0) as u128))
        .ok_or(VaultError::MathOverflow)?
        / (BPS_DENOM * SECONDS_PER_YEAR as u128);
    u64::try_from(v).map_err(|_| VaultError::MathOverflow.into())
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct AccrueFees<'info> {
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,

    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,

    /// Pinned to the creator. This instruction is permissionless, so without the
    /// authority check any caller could route the creator's fee into their own account.
    #[account(mut, token::mint = share_mint, token::authority = basket.creator)]
    pub creator_shares: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct SyncUnits<'info> {
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    pub share_mint: InterfaceAccount<'info, MintI>,
}

#[derive(Accounts)]
pub struct SyncListing<'info> {
    #[account(mut)]
    pub basket: Account<'info, Basket>,
}
