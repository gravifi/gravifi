//! Stop losses: a creator sets a stop on an asset, and once its posted price has stayed below the
//! stop for STOP_CONFIRM_SECS, the asset's target goes to 0% and the other assets' targets scale up
//! to fill the basket. Rebalancing then sells the asset out, in clips, like any asset set to 0%.
//!
//! On an immutable basket stops are part of the recipe: written before its first deposit, never
//! changed. A stop firing there follows the rule fixed when it was created.

use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::instructions::basket::authorise_mandate;
use crate::state::*;

/// A stop as the creator writes it: how far below which reference, and whether the reference
/// trails the price up.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct StopArgs {
    pub mint: Pubkey,
    pub pct_bps: u16,
    pub trailing: bool,
    pub reference_e12: u64,
}

#[event]
pub struct StopArmed {
    pub basket: Pubkey,
    pub mint: Pubkey,
    pub price_e12: u64,
    pub level_e12: u64,
}

#[event]
pub struct StopDisarmed {
    pub basket: Pubkey,
    pub mint: Pubkey,
    pub price_e12: u64,
}

#[event]
pub struct StopFired {
    pub basket: Pubkey,
    pub mint: Pubkey,
    pub price_e12: u64,
    pub level_e12: u64,
    pub targets_before: Vec<u16>,
    pub targets_after: Vec<u16>,
}

/// Replaces the basket's stops with `stops` (none clears them all), each unarmed. The creator of
/// a creator-managed basket at any time; the creator of an immutable one only before its first
/// deposit. Creates the stop account the first time, the creator paying its rent.
pub fn set_stops(ctx: Context<SetStops>, stops: Vec<StopArgs>) -> Result<()> {
    let basket = &ctx.accounts.basket;
    if basket.control == ControlMode::Immutable {
        require_keys_eq!(ctx.accounts.creator.key(), basket.creator, VaultError::NotMandateAuthority);
        require!(!basket.seeded, VaultError::StopsFixed);
    } else {
        authorise_mandate(basket, &ctx.accounts.creator)?;
    }
    let active = basket.active();
    require!(stops.len() <= active.len(), VaultError::BadStop);
    for (i, s) in stops.iter().enumerate() {
        require!(active.iter().any(|c| c.mint == s.mint), VaultError::BadStop);
        require!(stops[..i].iter().all(|o| o.mint != s.mint), VaultError::BadStop);
        require!((MIN_STOP_BPS..=MAX_STOP_BPS).contains(&s.pct_bps) && s.reference_e12 > 0, VaultError::BadStop);
    }

    let basket_key = basket.key();
    let st = &mut ctx.accounts.stop_loss;
    st.bump = ctx.bumps.stop_loss;
    st.basket = basket_key;
    st.stops = stops
        .iter()
        .map(|s| Stop { mint: s.mint, pct_bps: s.pct_bps, trailing: s.trailing, reference_e12: s.reference_e12, armed_at: 0 })
        .collect();
    Ok(())
}

/// Closes a creator-managed basket's stop account, its rent back to the creator. An immutable
/// basket's stops never go away.
pub fn close_stops(ctx: Context<CloseStops>) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.creator)
}

/// Checks the stop on `mint` against its posted price. Permissionless: the price is the keeper's,
/// bounded by its guard, and what a stop does is fixed. Below the stop, the first check arms it
/// and a check STOP_CONFIRM_SECS or more later fires it; back above, it disarms. A trailing stop's
/// reference first rises to a higher price.
///
/// Firing sets the asset's target to 0% and scales every other target up in proportion, the
/// rounding left over going to the largest, and removes the stop.
pub fn check_stop(ctx: Context<CheckStop>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let mint = ctx.accounts.asset_price.mint;
    let price = ctx.accounts.asset_price.price_e12;
    require!(price > 0 && now - ctx.accounts.asset_price.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);

    let basket_key = ctx.accounts.basket.key();
    let st = &mut ctx.accounts.stop_loss;
    let k = st.stops.iter().position(|s| s.mint == mint).ok_or(VaultError::StopNotSet)?;
    let basket = &mut ctx.accounts.basket;
    let n = basket.constituent_count as usize;
    let i = basket.active().iter().position(|c| c.mint == mint).ok_or(VaultError::StopInactive)?;
    require!(basket.constituents[i].target_bps > 0, VaultError::StopInactive);

    let s = &mut st.stops[k];
    if s.trailing && price > s.reference_e12 {
        s.reference_e12 = price;
    }
    let level = s.level_e12();
    let level_e12 = u64::try_from(level).map_err(|_| VaultError::MathOverflow)?;
    if (price as u128) >= level {
        if s.armed_at != 0 {
            s.armed_at = 0;
            emit!(StopDisarmed { basket: basket_key, mint, price_e12: price });
        }
        return Ok(());
    }
    if s.armed_at == 0 {
        s.armed_at = now;
        emit!(StopArmed { basket: basket_key, mint, price_e12: price, level_e12 });
        return Ok(());
    }
    if now - s.armed_at < STOP_CONFIRM_SECS {
        return Ok(());
    }

    // Fire: the asset to 0%, the rest scaled up to fill the basket.
    let before: Vec<u16> = basket.active().iter().map(|c| c.target_bps).collect();
    let rest = BPS_DENOM as u32 - before[i] as u32;
    require!(rest > 0, VaultError::StopLastAsset);
    let mut after: Vec<u16> = before
        .iter()
        .enumerate()
        .map(|(j, t)| if j == i { 0 } else { (*t as u32 * BPS_DENOM as u32 / rest) as u16 })
        .collect();
    let sum: u32 = after.iter().map(|t| *t as u32).sum();
    let largest = (0..n).max_by_key(|j| (after[*j], std::cmp::Reverse(*j))).unwrap_or(0);
    after[largest] += (BPS_DENOM as u32 - sum) as u16;
    for (j, t) in after.iter().enumerate() {
        basket.constituents[j].target_bps = *t;
    }
    st.stops.remove(k);
    st.fired = st.fired.saturating_add(1);
    emit!(StopFired { basket: basket_key, mint, price_e12: price, level_e12, targets_before: before, targets_after: after });
    Ok(())
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct SetStops<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    pub basket: Account<'info, Basket>,
    #[account(init_if_needed, payer = creator, space = 8 + StopLoss::INIT_SPACE, seeds = [STOP_SEED, basket.key().as_ref()], bump)]
    pub stop_loss: Account<'info, StopLoss>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseStops<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    pub basket: Account<'info, Basket>,
    #[account(mut, close = creator, seeds = [STOP_SEED, basket.key().as_ref()], bump = stop_loss.bump, has_one = basket)]
    pub stop_loss: Account<'info, StopLoss>,
}

#[derive(Accounts)]
pub struct CheckStop<'info> {
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    #[account(mut, seeds = [STOP_SEED, basket.key().as_ref()], bump = stop_loss.bump, has_one = basket)]
    pub stop_loss: Account<'info, StopLoss>,
    #[account(seeds = [PRICE_SEED, asset_price.mint.as_ref()], bump = asset_price.bump)]
    pub asset_price: Account<'info, AssetPrice>,
}
