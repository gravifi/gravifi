//! Cross-chain trips (C1, the team's choice on 8 Nov 2026): value moving between a mixed basket's
//! Solana side and its Robinhood Chain side, the part. A Solana slice is one transaction; a trip
//! takes minutes (bridge, redeem, sell, bridge back, buy), so the program keeps count of it:
//!
//! 1. `trip_start`: the trip key (ROLE_TRIP) sends `sell_amount` of an overweight sleeve out to the
//!    bridge-out address (ROLE_TRIP_OUT, another key, named by the authority with a day's notice:
//!    a leaked trip key alone can't take what a trip moves), toward an underweight one, one of the two being a part (proved on chain
//!    as `verify_part` proves it). No size cap ("I don't want something limiting my baskets"): a
//!    trip is only bounded by what the drift between the two sleeves needs. The least it must bring
//!    back (`min_buy`) is fixed now, from the posted prices less the slippage allowed.
//! 2. While it's out (`TRIP_OPEN`): mints, slices, fee accrual and splits wait (the shares away
//!    aren't in the vaults, so a mint would buy at a discount); a redeem pays its slice of the vaults
//!    and takes an IOU for its slice of the trip.
//! 3. `trip_settle`: once at least `min_buy` is in the trip's account, it goes into the basket, less
//!    the IOUs' slice, which they claim (`trip_claim`). Anyone may settle.
//! 4. Past the deadline, with less than its floor back, anyone may mark it failed (`trip_fail`): it
//!    stays out (mints wait, redeems take IOUs), and no new trip starts (`TRIP_STUCK`) until the
//!    guardian or the authority clears it (`trip_clear`). Whatever arrives later settles with no
//!    floor, by the trip key, the bridge-out address, the guardian or the authority, and the IOUs
//!    claim what came back; with nothing back, `trip_clear` closes it and the basket reopens.
//! 5. A trip's id is the unix time it starts, so no address is ever reused; `trip_sweep` returns
//!    to the basket what reaches a trip's account after it settled or was cleared.
//!
//! The keeper's bridge and gas fees come out of the sold sleeve as a refund capped by the
//! `TRIP_REFUND_E12` param, shown with the trip's event.

use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::instructions::rebalance::oracle_floor;
use crate::instructions::smart::prove_part;
use crate::params::{self, Params, PARAMS_ID};
use crate::state::*;
use crate::token_util::*;
use crate::valuation::*;

#[event]
pub struct TripStarted {
    pub basket: Pubkey,
    pub trip: Pubkey,
    pub sell_mint: Pubkey,
    pub buy_mint: Pubkey,
    pub sell_amount: u64,
    pub refund: u64,
    pub min_buy: u64,
    pub deadline: i64,
}

#[event]
pub struct TripSettled {
    pub basket: Pubkey,
    pub trip: Pubkey,
    pub delivered: u64,
    pub iou_pool: u64,
    /// Settled after it was marked failed: no floor.
    pub late: bool,
}

#[event]
pub struct TripFailed {
    pub basket: Pubkey,
    pub trip: Pubkey,
}

/// Starts a trip from sleeve `sell_index` to sleeve `buy_index`, one of them a part (`part_store`,
/// `part_peer`, `remote_eid` prove it). Remaining accounts: [vault_ata, asset_price] per
/// constituent, in basket order, as a slice takes them.
pub fn trip_start<'info>(
    ctx: Context<'_, '_, 'info, 'info, TripStart<'info>>,
    id: u32,
    sell_index: u8,
    buy_index: u8,
    sell_amount: u64,
    max_slippage_bps: u16,
    refund: u64,
    remote_eid: u32,
) -> Result<()> {
    let prm = Params::load(&ctx.accounts.params)?;
    prm.require_on(params::FAMILY_REBALANCE)?;
    let parts = ctx.accounts.parts.to_account_info();
    let key = trip_key_of(&parts);
    require!(key != Pubkey::default() && key == ctx.accounts.keeper.key(), VaultError::NotTripKeeper);
    // The id is the start's own unix time: an id used before is always more than a deadline old, so
    // no trip ever reuses another's address (and its IOUs).
    require!((id as i64) >= prm.now - TRIP_ID_WINDOW_SECS && (id as i64) <= prm.now + 60, VaultError::BadTripId);
    let basket = &ctx.accounts.basket;
    require!(basket.seeded, VaultError::NotSeeded);
    require!(!basket.flag(TRIP_OPEN), VaultError::TripOpen);
    require!(!basket.flag(TRIP_STUCK), VaultError::TripStuck);
    let n = basket.constituent_count as usize;
    let (s, b) = (sell_index as usize, buy_index as usize);
    require!(s < n && b < n && s != b && sell_amount > 0, VaultError::BadParameter);
    require!(max_slippage_bps <= prm.bps(params::REBALANCE_SLIPPAGE_BPS), VaultError::BadParameter);

    // One side is a part.
    let part_mint = [&ctx.accounts.sell_mint, &ctx.accounts.buy_mint]
        .into_iter()
        .find(|m| {
            prove_part(&parts, &m.to_account_info(), &ctx.accounts.part_store.to_account_info(), &ctx.accounts.part_peer.to_account_info(), remote_eid).is_ok()
        });
    require!(part_mint.is_some(), VaultError::TripNeedsPart);

    // Toward the targets, and no further than they need: the overweight sleeve down to its
    // target, the underweight one up to its, whichever is nearer. No other cap.
    let book = value_book(basket, ctx.remaining_accounts, &prm)?;
    let drift = drift_bps(basket, &book)?;
    let leaving = basket.constituents[s].target_bps == 0 && book.rows[s].balance > 0;
    require!((drift[s] > MIN_TRADE_DRIFT_BPS || leaving) && drift[b] < -MIN_TRADE_DRIFT_BPS, VaultError::WrongDirection);
    let gap = if leaving { drift[s].max(0).min(-drift[b]).max(1) } else { drift[s].min(-drift[b]) };
    let room = (gap as u128).checked_mul(book.total).ok_or(VaultError::MathOverflow)? / BPS_DENOM + book.total / BPS_DENOM;
    let sell_value = token_value(sell_amount, book.rows[s].price_e12, book.rows[s].decimals)?;
    require!(sell_value <= room, VaultError::SliceTooLarge);
    require!(token_value(refund, book.rows[s].price_e12, book.rows[s].decimals)? <= prm.wide(params::TRIP_REFUND_E12), VaultError::RefundTooLarge);
    require!(sell_amount.checked_add(refund).ok_or(VaultError::MathOverflow)? <= book.rows[s].balance, VaultError::SliceTooLarge);

    require_keys_eq!(ctx.accounts.sell_mint.key(), basket.constituents[s].mint, VaultError::ConstituentMismatch);
    require_keys_eq!(ctx.accounts.buy_mint.key(), basket.constituents[b].mint, VaultError::ConstituentMismatch);
    let sell_fee = TransferFee::of(&ctx.accounts.sell_mint.to_account_info())?.effective_bps(sell_amount)?;
    let min_buy = oracle_floor(&prm, sell_value, max_slippage_bps, sell_fee, &ctx.accounts.buy_mint.to_account_info(), &book.rows[b])?;

    // Out to the bridge-out address's account, nowhere else.
    require_keys_eq!(ctx.accounts.sell_vault.key(), ctx.remaining_accounts[s * 2].key(), VaultError::ConstituentMismatch);
    require_keys_eq!(*ctx.accounts.sell_token_program.key, *ctx.accounts.sell_mint.owner, VaultError::ConstituentMismatch);
    let out = trip_out_of(&parts);
    require!(out != Pubkey::default(), VaultError::NotBridgeOut);
    let to = read_token_account(&ctx.accounts.keeper_sell.to_account_info())?;
    require_keys_eq!(to.owner, out, VaultError::NotBridgeOut);
    require_keys_eq!(to.mint, basket.constituents[s].mint, VaultError::ConstituentMismatch);
    let creator = basket.creator;
    let basket_id = basket.basket_id;
    let bump = [basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let decimals = book.rows[s].decimals;
    transfer_from_vault(
        &ctx.accounts.sell_mint.to_account_info(),
        &ctx.accounts.sell_vault.to_account_info(),
        &ctx.accounts.keeper_sell.to_account_info(),
        &ctx.accounts.basket.to_account_info(),
        &ctx.accounts.sell_token_program.to_account_info(),
        sell_amount + refund,
        decimals,
        &[&seeds],
    )?;

    let basket_key = ctx.accounts.basket.key();
    let deadline = prm.now + prm.secs(params::TRIP_DEADLINE_SECS);
    let t = &mut ctx.accounts.trip;
    t.bump = ctx.bumps.trip;
    t.basket = basket_key;
    t.id = id;
    t.keeper = key;
    t.started_at = prm.now;
    t.deadline = deadline;
    t.sell_index = sell_index;
    t.buy_index = buy_index;
    t.sell_mint = ctx.accounts.sell_mint.key();
    t.buy_mint = ctx.accounts.buy_mint.key();
    t.sell_amount = sell_amount;
    t.min_buy = min_buy;
    t.supply_at_start = ctx.accounts.share_mint.supply;
    t.status = TRIP_OPENED;
    ctx.accounts.basket.set_flag(TRIP_OPEN, true);
    emit!(TripStarted {
        basket: basket_key,
        trip: ctx.accounts.trip.key(),
        sell_mint: ctx.accounts.trip.sell_mint,
        buy_mint: ctx.accounts.trip.buy_mint,
        sell_amount,
        refund,
        min_buy,
        deadline,
    });
    Ok(())
}

/// Puts what the trip brought back into the basket: at least `min_buy` of the bought token in the
/// trip's account, unless it was marked failed (then whatever came). The IOUs' slice stays for them
/// (`trip_claim`). Re-derives both sleeves' units. Permissionless.
pub fn trip_settle(ctx: Context<TripSettle>) -> Result<()> {
    let t = &ctx.accounts.trip;
    require!(t.status == TRIP_OPENED || t.status == TRIP_FAILED, VaultError::TripNotOpen);
    let late = t.status == TRIP_FAILED;
    if late {
        // No floor once failed, so not anyone: a stranger could otherwise send a unit of dust and
        // settle on it, leaving the real proceeds nowhere to go and the IOUs nearly nothing. The
        // trip key, the bridge-out address (which delivers), the guardian or the authority settle
        // it once what's coming has arrived.
        let s = ctx.accounts.signer.key();
        let protocol = ctx.accounts.protocol.to_account_info();
        let guardian = guardian_of(&protocol);
        let parts = ctx.accounts.parts.to_account_info();
        let out = trip_out_of(&parts);
        let ok = s == t.keeper || (out != Pubkey::default() && s == out) || s == authority_of(&protocol) || (guardian != Pubkey::default() && s == guardian);
        require!(ok, VaultError::LateSettleNotAllowed);
    }
    let trip_key = t.key();
    let got = read_vault(&ctx.accounts.trip_vault.to_account_info(), &trip_key, &t.buy_mint)?.amount;
    require!(late || got >= t.min_buy, VaultError::BelowOracleFloor);
    require!(got > 0, VaultError::BelowOracleFloor);
    let iou_pool = u64::try_from((got as u128) * (t.iou_shares as u128) / (t.supply_at_start.max(1) as u128)).map_err(|_| VaultError::MathOverflow)?;
    let to_basket = got - iou_pool;

    let basket_key = ctx.accounts.basket.key();
    let id = t.id.to_le_bytes();
    let bump = [t.bump];
    let seeds: [&[u8]; 4] = [TRIP_SEED, basket_key.as_ref(), &id, &bump];
    let decimals = mint_decimals(&ctx.accounts.buy_mint.to_account_info())?;
    require_keys_eq!(ctx.accounts.buy_mint.key(), t.buy_mint, VaultError::ConstituentMismatch);
    require_keys_eq!(*ctx.accounts.buy_token_program.key, *ctx.accounts.buy_mint.owner, VaultError::ConstituentMismatch);
    read_vault(&ctx.accounts.buy_vault.to_account_info(), &basket_key, &t.buy_mint)?;
    if to_basket > 0 {
        transfer_from_vault(
            &ctx.accounts.buy_mint.to_account_info(),
            &ctx.accounts.trip_vault.to_account_info(),
            &ctx.accounts.buy_vault.to_account_info(),
            &ctx.accounts.trip.to_account_info(),
            &ctx.accounts.buy_token_program.to_account_info(),
            to_basket,
            decimals,
            &[&seeds],
        )?;
    }

    // Both sleeves' units, from the vaults and the supply now.
    let supply = ctx.accounts.share_mint.supply;
    let (s, b) = (ctx.accounts.trip.sell_index as usize, ctx.accounts.trip.buy_index as usize);
    let sell_mint = ctx.accounts.trip.sell_mint;
    let buy_mint = ctx.accounts.trip.buy_mint;
    let sell_held = read_vault(&ctx.accounts.sell_vault.to_account_info(), &basket_key, &sell_mint)?.amount;
    let buy_held = read_vault(&ctx.accounts.buy_vault.to_account_info(), &basket_key, &buy_mint)?.amount;
    let basket = &mut ctx.accounts.basket;
    // What it bought must still be the sleeve it was bought for: never tokens the basket doesn't count.
    require!(basket.constituents.get(b).is_some_and(|c| c.mint == buy_mint), VaultError::ConstituentMismatch);
    if supply > 0 {
        if basket.constituents.get(s).is_some_and(|c| c.mint == sell_mint) {
            basket.constituents[s].units_per_share = Basket::live_units(sell_held, supply, basket.constituents[s].units_per_share)?;
        }
        if basket.constituents.get(b).is_some_and(|c| c.mint == buy_mint) {
            basket.constituents[b].units_per_share = Basket::live_units(buy_held, supply, basket.constituents[b].units_per_share)?;
        }
    }
    basket.set_flag(TRIP_OPEN, false);

    let t = &mut ctx.accounts.trip;
    t.status = TRIP_SETTLED;
    t.delivered = got;
    t.iou_pool = iou_pool;
    t.iou_pool_left = iou_pool;
    emit!(TripSettled { basket: basket_key, trip: trip_key, delivered: got, iou_pool, late });
    if t.iou_unclaimed == 0 {
        // Nothing kept for IOUs: the trip's token account is empty, and closes with the trip, its
        // rent back to the keeper (it would otherwise stay locked under an owner that's gone). One
        // that can't close yet (Token-2022 fees withheld) keeps the trip, for `trip_sweep`.
        let closed = close_if_empty(
            &ctx.accounts.trip_vault.to_account_info(),
            &ctx.accounts.keeper.to_account_info(),
            &ctx.accounts.trip.to_account_info(),
            &ctx.accounts.buy_token_program.to_account_info(),
            &[&seeds],
        )?;
        if closed {
            return ctx.accounts.trip.close(ctx.accounts.keeper.to_account_info());
        }
    }
    Ok(())
}

/// Marks a trip failed once past its deadline, with less than its floor back: the floor no longer
/// holds a late settle, and no new trip starts until the guardian or the authority clears it. It
/// stays out (`TRIP_OPEN`): mints still wait, as the basket's vaults miss what's away, and redeems
/// still take IOUs. A trip whose floor already arrived settles instead. Permissionless.
pub fn trip_fail(ctx: Context<TripFail>) -> Result<()> {
    let t = &ctx.accounts.trip;
    require!(t.status == TRIP_OPENED, VaultError::TripNotOpen);
    require!(Clock::get()?.unix_timestamp > t.deadline, VaultError::TripNotDue);
    // What the trip's token account holds (none when it was never opened).
    let vault = ctx.accounts.trip_vault.to_account_info();
    let trip_key = t.key();
    let at = |program: &Pubkey| Pubkey::find_program_address(&[trip_key.as_ref(), program.as_ref(), t.buy_mint.as_ref()], &ATA_PROGRAM_ID).0;
    require!(*vault.key == at(&anchor_spl::token::ID) || *vault.key == at(&anchor_spl::token_2022::ID), VaultError::BadRemainingAccounts);
    let held = if vault.data_len() > 0 { read_vault(&vault, &trip_key, &t.buy_mint)?.amount } else { 0 };
    require!(held < t.min_buy, VaultError::TripDelivered);
    let t = &mut ctx.accounts.trip;
    t.status = TRIP_FAILED;
    let basket = &mut ctx.accounts.basket;
    basket.set_flag(TRIP_STUCK, true);
    emit!(TripFailed { basket: basket.key(), trip: t.key() });
    Ok(())
}

/// Lets trips start again after one failed. While the failed trip is still out (nothing came back:
/// `TRIP_OPEN`), it must be named: it's closed, the rent back to its keeper, its IOUs then hold
/// nothing, and the basket reopens. A trip that settled late has reopened the basket and closed
/// itself, or stays for its IOUs: then only `TRIP_STUCK` clears. The guardian or the authority.
pub fn trip_clear(ctx: Context<TripClear>) -> Result<()> {
    let signer = ctx.accounts.signer.key();
    let protocol = ctx.accounts.protocol.to_account_info();
    let guardian = guardian_of(&protocol);
    require!(signer == authority_of(&protocol) || (guardian != Pubkey::default() && signer == guardian), VaultError::NotPauser);
    require!(ctx.accounts.basket.flag(TRIP_STUCK), VaultError::TripNotOpen);
    ctx.accounts.basket.set_flag(TRIP_STUCK, false);
    if !ctx.accounts.basket.flag(TRIP_OPEN) {
        return Ok(());
    }
    // The one trip out, failed, with nothing back.
    ctx.accounts.basket.set_flag(TRIP_OPEN, false);
    {
        let trip = ctx.accounts.trip.as_mut().ok_or(VaultError::TripStillOpen)?;
        require_keys_eq!(trip.basket, ctx.accounts.basket.key(), VaultError::TripNotOpen);
        require!(trip.status == TRIP_FAILED, VaultError::TripNotOpen);
        let keeper = ctx.accounts.keeper.as_ref().ok_or(VaultError::TripNotOpen)?;
        require_keys_eq!(keeper.key(), trip.keeper, VaultError::TripNotOpen);
        // Only a trip nothing came back to: its token account (the canonical one for what it was
        // to buy) is missing or empty. With tokens in it, it settles (late) instead, so its IOUs
        // get their slice; an empty one closes with the trip, its rent to the keeper.
        let vault = ctx.accounts.trip_vault.as_ref().ok_or(VaultError::BadRemainingAccounts)?.to_account_info();
        let trip_key = trip.key();
        let at = |program: &Pubkey| Pubkey::find_program_address(&[trip_key.as_ref(), program.as_ref(), trip.buy_mint.as_ref()], &ATA_PROGRAM_ID).0;
        require!(*vault.key == at(&anchor_spl::token::ID) || *vault.key == at(&anchor_spl::token_2022::ID), VaultError::BadRemainingAccounts);
        if vault.owner == &anchor_spl::token::ID || vault.owner == &anchor_spl::token_2022::ID {
            let view = read_token_account(&vault)?;
            require!(view.amount == 0, VaultError::TripVaultNotEmpty);
            let program = ctx.accounts.token_program.as_ref().ok_or(VaultError::BadRemainingAccounts)?.to_account_info();
            let id = trip.id.to_le_bytes();
            let bump = [trip.bump];
            let basket_key = ctx.accounts.basket.key();
            let seeds: [&[u8]; 4] = [TRIP_SEED, basket_key.as_ref(), &id, &bump];
            // Token-2022 fees withheld in it: harvest them to the mint first.
            require!(close_if_empty(&vault, &keeper.to_account_info(), &trip.to_account_info(), &program, &[&seeds])?, VaultError::TripVaultNotEmpty);
        } else {
            require!(vault.data_len() == 0, VaultError::BadRemainingAccounts);
        }
        trip.close(keeper.to_account_info())
    }
}

/// Moves whatever is left in a trip's token account to the basket's vault for that token, and
/// closes what it can: a delivery that came after the trip settled or was cleared, or a settled
/// trip whose account couldn't close (Token-2022 fees withheld: harvest first). Only once the trip
/// is gone, or settled with every IOU paid; its rent then goes to its keeper, the token account's to
/// the caller. Permissionless: it only returns the basket's own.
pub fn trip_sweep(ctx: Context<TripSweep>, id: u32) -> Result<()> {
    let basket_key = ctx.accounts.basket.key();
    let id_bytes = id.to_le_bytes();
    let (trip_key, bump) = Pubkey::find_program_address(&[TRIP_SEED, basket_key.as_ref(), &id_bytes], &crate::ID);
    let trip_ai = ctx.accounts.trip.to_account_info();
    require_keys_eq!(*trip_ai.key, trip_key, VaultError::TripNotOpen);
    let mint = ctx.accounts.mint.to_account_info();
    let program = ctx.accounts.token_program.to_account_info();
    require_keys_eq!(*program.key, *mint.owner, VaultError::ConstituentMismatch);
    let exists = trip_ai.owner == &crate::ID && trip_ai.data_len() > 0;
    if exists {
        let t = Trip::try_deserialize(&mut &trip_ai.try_borrow_data()?[..]).map_err(|_| error!(VaultError::TripNotOpen))?;
        require!(t.basket == basket_key && t.status == TRIP_SETTLED && t.iou_unclaimed == 0, VaultError::TripStillOpen);
        require_keys_eq!(t.buy_mint, *mint.key, VaultError::ConstituentMismatch);
        require_keys_eq!(ctx.accounts.keeper.key(), t.keeper, VaultError::TripNotOpen);
    }
    let vault = ctx.accounts.trip_vault.to_account_info();
    let held = read_vault(&vault, &trip_key, mint.key)?.amount;
    let b = [bump];
    let seeds: [&[u8]; 4] = [TRIP_SEED, basket_key.as_ref(), &id_bytes, &b];
    if held > 0 {
        let to = ctx.accounts.basket_vault.to_account_info();
        read_vault(&to, &basket_key, mint.key)?;
        transfer_from_vault(&mint, &vault, &to, &trip_ai, &program, held, mint_decimals(&mint)?, &[&seeds])?;
    }
    let closed = close_if_empty(&vault, &ctx.accounts.signer.to_account_info(), &trip_ai, &program, &[&seeds])?;
    if exists && closed {
        // The settled trip's account, its rent to its keeper.
        let keeper = ctx.accounts.keeper.to_account_info();
        let lamports = trip_ai.lamports();
        **trip_ai.try_borrow_mut_lamports()? = 0;
        **keeper.try_borrow_mut_lamports()? = keeper.lamports().checked_add(lamports).ok_or(VaultError::MathOverflow)?;
        trip_ai.resize(0)?;
        trip_ai.assign(&anchor_lang::solana_program::system_program::ID);
    }
    Ok(())
}

/// Gives a redeemer back the rent of an IOU whose trip is gone: one the guardian or the authority
/// cleared after nothing came back (the IOU's slice was of nothing). The wallet signs.
pub fn trip_iou_close(ctx: Context<TripIouClose>) -> Result<()> {
    let trip = &ctx.accounts.trip;
    require!(trip.data_len() == 0 && trip.owner != &crate::ID, VaultError::TripStillOpen);
    Ok(())
}

/// Pays a redeemer their slice of what a trip brought back, for the shares they redeemed while it
/// was out. The IOU closes, its rent back to them; the last one closes the trip.
pub fn trip_claim(ctx: Context<TripClaim>) -> Result<()> {
    let t = &ctx.accounts.trip;
    require!(t.status == TRIP_SETTLED, VaultError::TripNotSettled);
    let shares = ctx.accounts.iou.shares;
    let last = shares >= t.iou_unclaimed;
    // The last IOU takes what's left in the trip's account (its pool, and anything sent there
    // since), so the account can close with the trip.
    let held = read_vault(&ctx.accounts.trip_vault.to_account_info(), &t.key(), &t.buy_mint)?.amount;
    let pay = if last {
        held
    } else {
        u64::try_from((t.iou_pool as u128) * (shares as u128) / (t.iou_shares.max(1) as u128)).map_err(|_| VaultError::MathOverflow)?.min(t.iou_pool_left)
    };
    let basket_key = t.basket;
    let trip_key = t.key();
    require_keys_eq!(ctx.accounts.buy_mint.key(), t.buy_mint, VaultError::ConstituentMismatch);
    require_keys_eq!(*ctx.accounts.buy_token_program.key, *ctx.accounts.buy_mint.owner, VaultError::ConstituentMismatch);
    read_vault(&ctx.accounts.trip_vault.to_account_info(), &trip_key, &t.buy_mint)?;
    let to = read_token_account(&ctx.accounts.wallet_buy.to_account_info())?;
    require_keys_eq!(to.owner, ctx.accounts.wallet.key(), VaultError::ConstituentMismatch);
    if pay > 0 {
        let id = t.id.to_le_bytes();
        let bump = [t.bump];
        let seeds: [&[u8]; 4] = [TRIP_SEED, basket_key.as_ref(), &id, &bump];
        let decimals = mint_decimals(&ctx.accounts.buy_mint.to_account_info())?;
        transfer_from_vault(
            &ctx.accounts.buy_mint.to_account_info(),
            &ctx.accounts.trip_vault.to_account_info(),
            &ctx.accounts.wallet_buy.to_account_info(),
            &ctx.accounts.trip.to_account_info(),
            &ctx.accounts.buy_token_program.to_account_info(),
            pay,
            decimals,
            &[&seeds],
        )?;
    }
    let t = &mut ctx.accounts.trip;
    t.iou_pool_left = t.iou_pool_left.saturating_sub(pay);
    t.iou_unclaimed = t.iou_unclaimed.saturating_sub(shares);
    if last {
        let id = t.id.to_le_bytes();
        let bump = [t.bump];
        let seeds: [&[u8]; 4] = [TRIP_SEED, basket_key.as_ref(), &id, &bump];
        let closed = close_if_empty(
            &ctx.accounts.trip_vault.to_account_info(),
            &ctx.accounts.keeper.to_account_info(),
            &ctx.accounts.trip.to_account_info(),
            &ctx.accounts.buy_token_program.to_account_info(),
            &[&seeds],
        )?;
        // A token account that can't close yet keeps the trip, for `trip_sweep`.
        if closed {
            return ctx.accounts.trip.close(ctx.accounts.keeper.to_account_info());
        }
    }
    Ok(())
}

/// Called by `redeem_in_kind` while a trip is out: `trip`, `iou` and the system program are the
/// last three of its extra accounts. Adds `shares` (burned) to the redeemer's IOU, created the
/// first time with the redeemer paying its rent.
pub(crate) fn record_iou<'info>(basket: &Pubkey, trip_ai: &'info AccountInfo<'info>, iou_ai: &'info AccountInfo<'info>, system: &AccountInfo<'info>, payer: &AccountInfo<'info>, shares: u64) -> Result<()> {
    require_keys_eq!(*system.key, anchor_lang::system_program::ID, VaultError::BadRemainingAccounts);
    require_keys_eq!(*trip_ai.owner, crate::ID, VaultError::TripNotOpen);
    let mut trip: Account<Trip> = Account::try_from(trip_ai).map_err(|_| error!(VaultError::TripNotOpen))?;
    // Out, or failed and not yet settled late or cleared: the basket's vaults miss its value either way.
    require!(trip.basket == *basket && (trip.status == TRIP_OPENED || trip.status == TRIP_FAILED), VaultError::TripNotOpen);
    let (expected, bump) = Pubkey::find_program_address(&[TRIP_IOU_SEED, trip_ai.key.as_ref(), payer.key.as_ref()], &crate::ID);
    require_keys_eq!(*iou_ai.key, expected, VaultError::BadRemainingAccounts);
    if iou_ai.owner != &crate::ID {
        create_pda(payer, iou_ai, system, &[TRIP_IOU_SEED, trip_ai.key.as_ref(), payer.key.as_ref(), &[bump]], 8 + TripIou::INIT_SPACE)?;
        let iou = TripIou { bump, trip: *trip_ai.key, wallet: *payer.key, shares };
        let mut data = iou_ai.try_borrow_mut_data()?;
        let mut out: &mut [u8] = &mut data;
        iou.try_serialize(&mut out)?;
    } else {
        let mut iou: Account<TripIou> = Account::try_from(iou_ai)?;
        iou.shares = iou.shares.checked_add(shares).ok_or(VaultError::MathOverflow)?;
        iou.exit(&crate::ID)?;
    }
    trip.iou_shares = trip.iou_shares.checked_add(shares).ok_or(VaultError::MathOverflow)?;
    trip.iou_unclaimed = trip.iou_unclaimed.checked_add(shares).ok_or(VaultError::MathOverflow)?;
    trip.exit(&crate::ID)
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
#[instruction(id: u32)]
pub struct TripStart<'info> {
    /// The trip key (ROLE_TRIP); pays the trip's rent.
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: `[PARTS_SEED]`, read by hand (`trip_key_of`, `prove_part`).
    #[account(seeds = [PARTS_SEED], bump)]
    pub parts: UncheckedAccount<'info>,
    #[account(mut, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, anchor_spl::token_interface::Mint>>,
    #[account(init, payer = keeper, space = 8 + Trip::INIT_SPACE, seeds = [TRIP_SEED, basket.key().as_ref(), &id.to_le_bytes()], bump)]
    pub trip: Box<Account<'info, Trip>>,
    /// CHECK: matched against the sold constituent.
    pub sell_mint: UncheckedAccount<'info>,
    /// CHECK: matched against the bought constituent.
    pub buy_mint: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for the sold token, matched against the remaining accounts.
    #[account(mut)]
    pub sell_vault: UncheckedAccount<'info>,
    /// CHECK: a token account of the bridge-out address's, for the sold token, checked in the instruction.
    #[account(mut)]
    pub keeper_sell: UncheckedAccount<'info>,
    /// CHECK: the sold mint's owner, checked in the instruction.
    pub sell_token_program: UncheckedAccount<'info>,
    /// CHECK: the part's OFT store, read by `prove_part`.
    pub part_store: UncheckedAccount<'info>,
    /// CHECK: the store's peer on Robinhood Chain, read by `prove_part`.
    pub part_peer: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: the params account (`params.rs`); it may not exist, and then every value is its default.
    #[account(address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TripSettle<'info> {
    pub signer: Signer<'info>,
    #[account(mut, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, anchor_spl::token_interface::Mint>>,
    #[account(mut, has_one = basket)]
    pub trip: Box<Account<'info, Trip>>,
    /// CHECK: the trip's keeper, who gets its rent back if it closes now.
    #[account(mut, address = trip.keeper)]
    pub keeper: UncheckedAccount<'info>,
    /// CHECK: the bought mint, matched against the trip.
    pub buy_mint: UncheckedAccount<'info>,
    /// CHECK: the trip's canonical account for the bought token, checked by `read_vault`.
    #[account(mut)]
    pub trip_vault: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for the bought token, checked by `read_vault`.
    #[account(mut)]
    pub buy_vault: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for the sold token, checked by `read_vault` (read only).
    pub sell_vault: UncheckedAccount<'info>,
    /// CHECK: the bought mint's owner, checked in the instruction.
    pub buy_token_program: UncheckedAccount<'info>,
    /// Read for the guardian and the authority, who may settle a failed trip.
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    /// CHECK: `[PARTS_SEED]`, read by hand for the bridge-out address (`trip_out_of`).
    #[account(seeds = [PARTS_SEED], bump)]
    pub parts: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TripFail<'info> {
    #[account(mut)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(mut, has_one = basket)]
    pub trip: Box<Account<'info, Trip>>,
    /// CHECK: the trip's canonical token account for what it was to buy (it may not exist), checked
    /// in the instruction: one that already holds the floor settles instead.
    pub trip_vault: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TripSweep<'info> {
    /// Gets the trip token account's rent, if it closes.
    #[account(mut)]
    pub signer: Signer<'info>,
    pub basket: Box<Account<'info, Basket>>,
    /// CHECK: `[TRIP_SEED, basket, id]`, checked in the instruction; it may be gone.
    #[account(mut)]
    pub trip: UncheckedAccount<'info>,
    /// CHECK: the trip's canonical token account for `mint`, checked by `read_vault`.
    #[account(mut)]
    pub trip_vault: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for `mint`, checked by `read_vault`.
    #[account(mut)]
    pub basket_vault: UncheckedAccount<'info>,
    /// CHECK: the token's mint; its owner is the token program.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: the mint's owner, checked in the instruction.
    pub token_program: UncheckedAccount<'info>,
    /// CHECK: the trip's keeper, who gets a settled trip's rent back; any account once the trip is gone.
    #[account(mut)]
    pub keeper: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TripClear<'info> {
    pub signer: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    #[account(mut)]
    pub basket: Box<Account<'info, Basket>>,
    /// The failed trip, to close it; left out once it settled.
    #[account(mut)]
    pub trip: Option<Box<Account<'info, Trip>>>,
    /// CHECK: the trip's keeper, who gets its rent back; checked in the instruction.
    #[account(mut)]
    pub keeper: Option<UncheckedAccount<'info>>,
    /// CHECK: the trip's canonical token account for what it was to buy (missing or empty), with
    /// the trip; checked in the instruction.
    #[account(mut)]
    pub trip_vault: Option<UncheckedAccount<'info>>,
    /// CHECK: that account's token program, when it exists; matched as it closes.
    pub token_program: Option<UncheckedAccount<'info>>,
}

#[derive(Accounts)]
pub struct TripIouClose<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, close = wallet, seeds = [TRIP_IOU_SEED, iou.trip.as_ref(), wallet.key().as_ref()], bump = iou.bump, has_one = wallet)]
    pub iou: Box<Account<'info, TripIou>>,
    /// CHECK: the IOU's trip, which must be gone.
    #[account(address = iou.trip)]
    pub trip: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TripClaim<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut)]
    pub trip: Box<Account<'info, Trip>>,
    #[account(mut, close = wallet, seeds = [TRIP_IOU_SEED, trip.key().as_ref(), wallet.key().as_ref()], bump = iou.bump, has_one = trip, has_one = wallet)]
    pub iou: Box<Account<'info, TripIou>>,
    /// CHECK: the trip's keeper, who gets its rent back when the last IOU is paid.
    #[account(mut, address = trip.keeper)]
    pub keeper: UncheckedAccount<'info>,
    /// CHECK: the bought mint, matched against the trip.
    pub buy_mint: UncheckedAccount<'info>,
    /// CHECK: the trip's canonical account for the bought token, checked by `read_vault`.
    #[account(mut)]
    pub trip_vault: UncheckedAccount<'info>,
    /// CHECK: the wallet's account for the bought token, checked in the instruction.
    #[account(mut)]
    pub wallet_buy: UncheckedAccount<'info>,
    /// CHECK: the bought mint's owner, checked in the instruction.
    pub buy_token_program: UncheckedAccount<'info>,
}
