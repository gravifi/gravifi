//! Stop losses: a creator sets a stop on an asset, and once its posted price has stayed below the
//! stop for STOP_CONFIRM_SECS, the asset's target goes to 0% and the other assets' targets scale up
//! to fill the basket. Rebalancing then sells the asset out, in clips, like any asset set to 0%.
//!
//! On an immutable basket stops are part of the recipe: written before its first deposit, never
//! changed. A stop firing there follows the rule fixed when it was created.
//!
//! A stop's reference is never above its asset's fresh posted price when it's written on a seeded
//! basket, so no stop starts out armed: otherwise a smart basket's creator (or a vote) could write
//! one far above the price and have it fire five minutes later, moving targets around the
//! guardrails and the mandate's notice. Before the first deposit there are no holders whose basket
//! a stop can change, every buyer sees the stops, and an asset may have no price posted yet, so
//! references aren't checked then. On a smart or vote-run basket a firing stop also keeps every
//! other asset within the guardrails' weight caps (`rescale`).

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
///
/// Once the basket is seeded, remaining accounts: each stop's `AssetPrice`, in the order of
/// `stops`, fresh; no reference may be above its price.
pub fn set_stops<'info>(ctx: Context<'_, '_, 'info, 'info, SetStops<'info>>, mut stops: Vec<StopArgs>) -> Result<()> {
    let basket = &ctx.accounts.basket;
    if basket.control == ControlMode::Immutable {
        require_keys_eq!(ctx.accounts.creator.key(), basket.creator, VaultError::NotMandateAuthority);
        require!(!basket.seeded, VaultError::StopsFixed);
    } else {
        authorise_mandate(basket, &ctx.accounts.creator)?;
    }
    check_stops(basket, &stops)?;
    if basket.seeded {
        hold_to_price(&mut stops, ctx.remaining_accounts, false)?;
    }
    let basket_key = basket.key();
    write_stops(&mut ctx.accounts.stop_loss, basket_key, ctx.bumps.stop_loss, &stops);
    Ok(())
}

/// One stop per asset at most, on assets the basket holds, in range.
pub(crate) fn check_stops(basket: &Basket, stops: &[StopArgs]) -> Result<()> {
    let active = basket.active();
    require!(stops.len() <= active.len(), VaultError::BadStop);
    for (i, s) in stops.iter().enumerate() {
        require!(active.iter().any(|c| c.mint == s.mint), VaultError::BadStop);
        require!(stops[..i].iter().all(|o| o.mint != s.mint), VaultError::BadStop);
        require!((MIN_STOP_BPS..=MAX_STOP_BPS).contains(&s.pct_bps) && s.reference_e12 > 0, VaultError::BadStop);
    }
    Ok(())
}

/// Holds each stop's reference to its asset's fresh posted price (`prices`: each stop's
/// `AssetPrice`, in order): one above it is refused, or with `lower`, lowered to it (a vote's
/// stops, written days after they were proposed, can't be proposed again).
pub(crate) fn hold_to_price(stops: &mut [StopArgs], prices: &[AccountInfo], lower: bool) -> Result<()> {
    require!(prices.len() == stops.len(), VaultError::BadRemainingAccounts);
    let now = Clock::get()?.unix_timestamp;
    for (s, ai) in stops.iter_mut().zip(prices) {
        let price = fresh_price(ai, &s.mint, now)?;
        if s.reference_e12 > price {
            require!(lower, VaultError::StopAbovePrice);
            s.reference_e12 = price;
        }
    }
    Ok(())
}

/// `mint`'s posted price, read from its `AssetPrice` at `ai`, if fresh.
fn fresh_price(ai: &AccountInfo, mint: &Pubkey, now: i64) -> Result<u64> {
    require_keys_eq!(*ai.owner, crate::ID, VaultError::BadPrice);
    let p = AssetPrice::try_deserialize(&mut &ai.try_borrow_data()?[..])?;
    require_keys_eq!(p.mint, *mint, VaultError::BadPrice);
    let expected = Pubkey::create_program_address(&[PRICE_SEED, mint.as_ref(), &[p.bump]], &crate::ID).map_err(|_| VaultError::BadPrice)?;
    require_keys_eq!(*ai.key, expected, VaultError::BadPrice);
    require!(p.price_e12 > 0 && now - p.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
    Ok(p.price_e12)
}

/// Replaces a basket's stops, each unarmed.
pub(crate) fn write_stops(st: &mut StopLoss, basket: Pubkey, bump: u8, stops: &[StopArgs]) {
    st.bump = bump;
    st.basket = basket;
    st.stops = stops
        .iter()
        .map(|s| Stop { mint: s.mint, pct_bps: s.pct_bps, trailing: s.trailing, reference_e12: s.reference_e12, armed_at: 0 })
        .collect();
}

/// Closes a creator-managed basket's stop account, its rent back to the creator. An immutable
/// basket's stops never go away.
pub fn close_stops(ctx: Context<CloseStops>) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.creator)
}

/// Checks the stop on `mint` against its posted price. Permissionless: the price is the keeper's,
/// bounded by its guard, and what a stop does is fixed. Below the stop, a check arms it and a check
/// STOP_CONFIRM_SECS to STOP_ARM_WINDOW_SECS later fires it; an arming older than that starts
/// over; back above, it disarms.
///
/// A trailing stop's reference first rises to a higher price that has held: the lower of the
/// posted price and the asset's `PriceGuard` reference, once that reference is at least
/// STOP_CONFIRM_SECS old. Without the guard among the remaining accounts it doesn't rise.
///
/// Firing sets the asset's target to 0% and scales every other target up in proportion, the
/// rounding left over going to the largest, and removes the stop. On a smart or vote-run basket no
/// asset is scaled past its guardrail weight cap (`rescale`): firing there needs, among the
/// remaining accounts, the basket's `Smart` account (a smart basket) and every asset's Verified
/// record address. Remaining accounts are found by address, in any order.
pub fn check_stop<'info>(ctx: Context<'_, '_, 'info, 'info, CheckStop<'info>>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let mint = ctx.accounts.asset_price.mint;
    let price = ctx.accounts.asset_price.price_e12;
    require!(price > 0 && now - ctx.accounts.asset_price.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);

    let basket_key = ctx.accounts.basket.key();
    let extra = ctx.remaining_accounts;
    let st = &mut ctx.accounts.stop_loss;
    let k = st.stops.iter().position(|s| s.mint == mint).ok_or(VaultError::StopNotSet)?;
    let basket = &mut ctx.accounts.basket;
    let i = basket.active().iter().position(|c| c.mint == mint).ok_or(VaultError::StopInactive)?;
    require!(basket.constituents[i].target_bps > 0, VaultError::StopInactive);

    let held = if st.stops[k].trailing { held_reference(extra, &mint, now)? } else { None };
    let s = &mut st.stops[k];
    if let Some(held) = held {
        let up = price.min(held);
        if up > s.reference_e12 {
            s.reference_e12 = up;
        }
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
    if s.armed_at == 0 || now - s.armed_at > STOP_ARM_WINDOW_SECS {
        s.armed_at = now;
        emit!(StopArmed { basket: basket_key, mint, price_e12: price, level_e12 });
        return Ok(());
    }
    if now - s.armed_at < STOP_CONFIRM_SECS {
        return Ok(());
    }

    // Fire: the asset to 0%, the rest scaled up to fill the basket, within the caps if any.
    let before: Vec<u16> = basket.active().iter().map(|c| c.target_bps).collect();
    let caps = weight_caps(basket, &basket_key, extra)?;
    let after = rescale(&before, i, caps.as_deref())?;
    for (j, t) in after.iter().enumerate() {
        basket.constituents[j].target_bps = *t;
    }
    st.stops.remove(k);
    st.fired = st.fired.saturating_add(1);
    emit!(StopFired { basket: basket_key, mint, price_e12: price, level_e12, targets_before: before, targets_after: after });
    Ok(())
}

/// The asset's `PriceGuard` reference, if its guard is among `extra` and the reference is at least
/// STOP_CONFIRM_SECS old: a price posted then that every price since has been held to.
fn held_reference(extra: &[AccountInfo], mint: &Pubkey, now: i64) -> Result<Option<u64>> {
    let (expected, _) = Pubkey::find_program_address(&[GUARD_SEED, mint.as_ref()], &crate::ID);
    let Some(ai) = extra.iter().find(|a| *a.key == expected) else { return Ok(None) };
    require_keys_eq!(*ai.owner, crate::ID, VaultError::BadPrice);
    let g = PriceGuard::try_deserialize(&mut &ai.try_borrow_data()?[..])?;
    Ok((g.reference_e12 > 0 && now - g.reference_at >= STOP_CONFIRM_SECS).then_some(g.reference_e12))
}

/// The most each asset may weigh once a stop fires: a smart basket's guardrail caps, a vote-run
/// one's the same code ceilings a Reweight is held to, and none (None) on any other basket. The
/// Verified records and the `Smart` account are found by address among `extra`; a missing one is
/// refused rather than guessed, so a firing can't be made to move less than its rule says.
fn weight_caps(basket: &Basket, basket_key: &Pubkey, extra: &[AccountInfo]) -> Result<Option<Vec<u16>>> {
    let (weight, unverified) = match basket.control {
        ControlMode::Smart => {
            let (expected, _) = Pubkey::find_program_address(&[SMART_SEED, basket_key.as_ref()], &crate::ID);
            let ai = extra.iter().find(|a| *a.key == expected).ok_or(VaultError::BadRemainingAccounts)?;
            require_keys_eq!(*ai.owner, crate::ID, VaultError::BadRemainingAccounts);
            let smart = Smart::try_deserialize(&mut &ai.try_borrow_data()?[..])?;
            (smart.guard.max_weight_bps, smart.guard.max_unverified_bps)
        }
        ControlMode::Community if basket.by_vote() => (SMART_MAX_WEIGHT_BPS, SMART_MAX_UNVERIFIED_BPS),
        _ => return Ok(None),
    };
    let mut caps = Vec::with_capacity(basket.constituent_count as usize);
    for c in basket.active() {
        let (expected, _) = Pubkey::find_program_address(&[VERIFIED_SEED, c.mint.as_ref()], &crate::ID);
        let ai = extra.iter().find(|a| *a.key == expected).ok_or(VaultError::BadRemainingAccounts)?;
        let verified = ai.owner == &crate::ID && ai.data_len() > 0;
        caps.push(if verified { weight } else { unverified });
    }
    Ok(Some(caps))
}

/// Targets once asset `i` is stopped out: it goes to 0% and every other asset with a target scales
/// up in proportion. With `caps`, none is scaled past the larger of its cap and its weight now; what
/// that leaves, and the rounding, go to the largest asset with a target and room, then the next.
/// What no asset has room for stays on the stopped asset, and a stop with no room at all is refused
/// (it stays armed for when there is).
pub(crate) fn rescale(before: &[u16], i: usize, caps: Option<&[u16]>) -> Result<Vec<u16>> {
    let rest = BPS_DENOM as u32 - before[i] as u32;
    require!(rest > 0, VaultError::StopLastAsset);
    let room = |j: usize| caps.map_or(BPS_DENOM as u32, |c| c[j].max(before[j]) as u32);
    let mut after: Vec<u32> = before
        .iter()
        .enumerate()
        .map(|(j, t)| if j == i { 0 } else { (*t as u32 * BPS_DENOM as u32 / rest).min(room(j)) })
        .collect();
    let mut left = BPS_DENOM as u32 - after.iter().sum::<u32>();
    while left > 0 {
        let open = (0..before.len()).filter(|j| *j != i && before[*j] > 0 && after[*j] < room(*j));
        let Some(j) = open.max_by_key(|j| (after[*j], std::cmp::Reverse(*j))) else { break };
        let add = left.min(room(j) - after[j]);
        after[j] += add;
        left -= add;
    }
    after[i] = left;
    require!(after[i] < before[i] as u32, VaultError::StopNoRoom);
    Ok(after.into_iter().map(|t| t as u16).collect())
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
