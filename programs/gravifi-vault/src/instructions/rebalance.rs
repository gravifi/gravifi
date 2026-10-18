//! Rebalancing, one slice at a time: the vault lends an overweight token to the rebalancer, who
//! swaps it in the same transaction, and the finish checks what came back against the oracle.
//! A sweep works the same way for a token the basket was sent but doesn't hold.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::{load_current_index_checked, load_instruction_at_checked};
use anchor_spl::token_interface::{Mint as MintI, TokenInterface};

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;
use crate::valuation::*;

/// `RebalanceState::sell_index` of a sweep, whose sold token isn't one of the basket's.
pub const SWEEP_INDEX: u8 = u8::MAX;

/// First half of one rebalance slice: lends `sell_amount` of an overweight sleeve to
/// the rebalancer, who swaps it on the market in the same transaction. The matching
/// `rebalance_finish` must follow in that transaction and checks the vault got back at
/// least the oracle value minus `max_slippage_bps` of the underweight sleeve.
///
/// Remaining accounts: [vault_ata, asset_price] per constituent, in basket order.
pub fn rebalance_start<'info>(
    ctx: Context<'_, '_, 'info, 'info, RebalanceStart<'info>>,
    sell_index: u8,
    buy_index: u8,
    sell_amount: u64,
    max_slippage_bps: u16,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let basket = &ctx.accounts.basket;
    require!(basket.seeded, VaultError::NotSeeded);
    let n = basket.constituent_count as usize;
    let (s, b) = (sell_index as usize, buy_index as usize);
    require!(s < n && b < n && s != b && sell_amount > 0, VaultError::BadParameter);
    require!(max_slippage_bps <= MAX_REBALANCE_SLIPPAGE_BPS, VaultError::BadParameter);
    require!(!ctx.accounts.rebalance_state.pending, VaultError::RebalancePending);

    let book = value_book(basket, ctx.remaining_accounts, now)?;
    let drift = drift_bps(basket, &book)?;
    let max_drift = drift.iter().map(|d| d.abs()).max().unwrap_or(0);
    let time_due = basket.interval_secs > 0 && now >= basket.last_rebalance.saturating_add(basket.interval_secs);
    let drift_due = max_drift > basket.drift_bps as i64;
    let due = match basket.rebalance_mode {
        RebalanceMode::Periodic => time_due,
        RebalanceMode::Drift => drift_due,
        RebalanceMode::Hybrid => time_due || drift_due,
    };
    // A sleeve its creator set to 0% is being taken out: it may trade whenever it holds
    // anything, into any sleeve that's under target, and its last EMPTYING_BPS go in one
    // slice ("emptying"), so it can then be removed.
    let leaving = basket.constituents[s].target_bps == 0 && book.rows[s].balance > 0;
    let emptying = leaving && drift[s] <= EMPTYING_BPS;
    // Likewise an asset its creator added, with a target but nothing bought yet: the first
    // slice opens a session, which then runs until every sleeve is on target.
    let joining = basket.active().iter().any(|c| c.units_per_share == 0 && c.target_bps > 0);
    // A session that hasn't traded for a day has lapsed: it only keeps the basket trading
    // outside its schedule while it's actually converging.
    let st = &ctx.accounts.rebalance_state;
    let session = st.session_active && Clock::get()?.slot.saturating_sub(st.pending_slot) <= SESSION_IDLE_SLOTS;
    require!(due || session || leaving || joining, VaultError::RebalanceNotDue);
    require!(
        (drift[s] > MIN_TRADE_DRIFT_BPS || emptying)
            && (drift[b] < -MIN_TRADE_DRIFT_BPS || (leaving && drift[b] < 0) || (emptying && drift[b] <= 0)),
        VaultError::WrongDirection
    );

    // Never past the target on either side, and never more than one slice of the basket.
    // Emptying may overshoot the bought sleeve by what's left, at most EMPTYING_BPS.
    let total = book.total;
    let sell_value = token_value(sell_amount, book.rows[s].price_e12, book.rows[s].decimals)?;
    let gap = if emptying { drift[s].max(0) } else { drift[s].min(-drift[b]) };
    let room = (gap as u128)
        .checked_mul(total)
        .ok_or(VaultError::MathOverflow)?
        / BPS_DENOM
        + total / BPS_DENOM;
    let cap = room.min(total * MAX_SLICE_BPS / BPS_DENOM);
    require!(sell_value <= cap, VaultError::SliceTooLarge);
    require!(sell_amount <= book.rows[s].balance, VaultError::SliceTooLarge);

    // The oracle floor: the least the vault accepts back, in the bought token's units.
    // Transfer fees aren't slippage: the lent tokens reach the rebalancer less the sold
    // token's fee, and the bought ones reach the vault less theirs, so the floor allows
    // for both. No new money goes into a sleeve whose fee is over the cap; selling one
    // out is always allowed.
    require_keys_eq!(ctx.accounts.buy_mint.key(), basket.constituents[b].mint, VaultError::ConstituentMismatch);
    let sell_fee = TransferFee::of(&ctx.accounts.sell_mint.to_account_info())?;
    let min_buy = oracle_floor(sell_value, max_slippage_bps, sell_fee.bps, &ctx.accounts.buy_mint.to_account_info(), &book.rows[b])?;

    let basket_key = basket.key();
    assert_finish_follows(&ctx.accounts.instructions.to_account_info(), &basket_key)?;

    // Lend the sleeve to the rebalancer.
    let c = basket.constituents[s];
    require_keys_eq!(ctx.accounts.sell_mint.key(), c.mint, VaultError::ConstituentMismatch);
    require_keys_eq!(ctx.accounts.sell_vault.key(), ctx.remaining_accounts[s * 2].key(), VaultError::ConstituentMismatch);
    require_keys_eq!(*ctx.accounts.sell_token_program.key, *ctx.accounts.sell_mint.owner, VaultError::ConstituentMismatch);
    let creator = basket.creator;
    let basket_id = basket.basket_id;
    let bump = [basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let decimals = book.rows[s].decimals;
    transfer_from_vault(
        &ctx.accounts.sell_mint.to_account_info(),
        &ctx.accounts.sell_vault.to_account_info(),
        &ctx.accounts.rebalancer_sell.to_account_info(),
        &ctx.accounts.basket.to_account_info(),
        &ctx.accounts.sell_token_program.to_account_info(),
        sell_amount,
        decimals,
        &[&seeds],
    )?;

    // Shares minted or redeemed between start and finish would pass for the swap's
    // proceeds (a deposit lands in the bought sleeve too). The finish requires the
    // supply it sees to be this one.
    ctx.accounts.basket.rebalance_supply = ctx.accounts.share_mint.supply;

    let st = &mut ctx.accounts.rebalance_state;
    st.bump = ctx.bumps.rebalance_state;
    st.basket = basket_key;
    st.session_active = true;
    st.pending = true;
    st.pending_slot = Clock::get()?.slot;
    st.sell_index = sell_index;
    st.buy_index = buy_index;
    st.sell_amount = sell_amount;
    st.min_buy = min_buy;
    st.buy_balance_before = book.rows[b].balance;
    Ok(())
}

/// Second half of a slice (or a sweep): checks the oracle floor, re-derives units for the
/// sleeves that traded, and ends the session once every sleeve is near target, or once no
/// trade `rebalance_start` would accept is left.
///
/// Remaining accounts: [vault_ata, asset_price] per constituent, as in the start.
pub fn rebalance_finish<'info>(ctx: Context<'_, '_, 'info, 'info, RebalanceFinish<'info>>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let st = &ctx.accounts.rebalance_state;
    require!(st.pending && st.pending_slot == Clock::get()?.slot, VaultError::NoRebalancePending);
    let (s, b) = (st.sell_index as usize, st.buy_index as usize);

    let book = value_book(&ctx.accounts.basket, ctx.remaining_accounts, now)?;
    let received = book.rows[b].balance.saturating_sub(st.buy_balance_before);
    require!(received >= st.min_buy, VaultError::BelowOracleFloor);
    let drift = drift_bps(&ctx.accounts.basket, &book)?;
    // Near target everywhere, or no pair left to trade: a slice needs one sleeve over by more
    // than MIN_TRADE_DRIFT_BPS and another under by as much, and a residue spread thinly
    // across many sleeves can leave one over target with no sleeve far enough under to buy.
    // Without the second test that session never ended.
    let near = drift.iter().all(|d| d.abs() < SESSION_END_DRIFT_BPS);
    let tradeable = drift.iter().any(|d| *d > MIN_TRADE_DRIFT_BPS) && drift.iter().any(|d| *d < -MIN_TRADE_DRIFT_BPS);
    let settled = near || !tradeable;
    let sweep = st.sell_index == SWEEP_INDEX;

    let supply = ctx.accounts.share_mint.supply;
    require!(supply > 0, VaultError::ZeroSupply);
    require!(supply == ctx.accounts.basket.rebalance_supply, VaultError::SupplyChanged);
    let basket = &mut ctx.accounts.basket;
    let traded: &[usize] = if sweep { &[b] } else { &[s, b] };
    for &i in traded {
        let units = (book.rows[i].balance as u128)
            .checked_mul(ONE_SHARE)
            .ok_or(VaultError::MathOverflow)?
            / supply as u128;
        basket.constituents[i].units_per_share = u64::try_from(units).map_err(|_| VaultError::MathOverflow)?;
    }

    let st = &mut ctx.accounts.rebalance_state;
    st.pending = false;
    st.slices = st.slices.saturating_add(1);
    if settled && !sweep {
        st.session_active = false;
        basket.last_rebalance = now;
    }
    Ok(())
}

/// Turns a token the basket was sent but doesn't hold (the ZEC a stonkfun token pays its
/// holders, an airdrop) into one of its own: lends `sell_amount` of it to the rebalancer, who
/// swaps it for constituent `buy_index` in the same transaction, and `rebalance_finish` checks
/// the oracle floor and raises that sleeve's units. Every holder's slice grows; nothing else
/// moves, and no rebalance session starts or ends.
///
/// Remaining accounts: [vault_ata, asset_price] per constituent, in basket order.
pub fn sweep_start<'info>(
    ctx: Context<'_, '_, 'info, 'info, SweepStart<'info>>,
    buy_index: u8,
    sell_amount: u64,
    max_slippage_bps: u16,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let basket = &ctx.accounts.basket;
    require!(basket.seeded, VaultError::NotSeeded);
    let b = buy_index as usize;
    require!(b < basket.constituent_count as usize && sell_amount > 0, VaultError::BadParameter);
    require!(max_slippage_bps <= MAX_REBALANCE_SLIPPAGE_BPS, VaultError::BadParameter);
    require!(!ctx.accounts.rebalance_state.pending, VaultError::RebalancePending);

    // Only a token outside the books: the basket's own are rebalanced, and its shares aren't
    // something to sell.
    let stray = ctx.accounts.stray_mint.key();
    require!(stray != basket.share_mint && basket.active().iter().all(|c| c.mint != stray), VaultError::NotStray);
    let basket_key = basket.key();
    let held = read_vault(&ctx.accounts.stray_vault.to_account_info(), &basket_key, &stray)?.amount;
    require!(sell_amount <= held, VaultError::SliceTooLarge);

    let price = &ctx.accounts.stray_price;
    require!(price.price_e12 > 0 && now - price.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
    let book = value_book(basket, ctx.remaining_accounts, now)?;
    require_keys_eq!(ctx.accounts.buy_mint.key(), basket.constituents[b].mint, VaultError::ConstituentMismatch);
    let sell_value = token_value(sell_amount, price.price_e12, ctx.accounts.stray_mint.decimals)?;
    let sell_fee = TransferFee::of(&ctx.accounts.stray_mint.to_account_info())?;
    let min_buy = oracle_floor(sell_value, max_slippage_bps, sell_fee.bps, &ctx.accounts.buy_mint.to_account_info(), &book.rows[b])?;

    assert_finish_follows(&ctx.accounts.instructions.to_account_info(), &basket_key)?;

    let creator = basket.creator;
    let basket_id = basket.basket_id;
    let bump = [basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    transfer_from_vault(
        &ctx.accounts.stray_mint.to_account_info(),
        &ctx.accounts.stray_vault.to_account_info(),
        &ctx.accounts.rebalancer_sell.to_account_info(),
        &ctx.accounts.basket.to_account_info(),
        &ctx.accounts.stray_token_program.to_account_info(),
        sell_amount,
        ctx.accounts.stray_mint.decimals,
        &[&seeds],
    )?;

    ctx.accounts.basket.rebalance_supply = ctx.accounts.share_mint.supply;
    let st = &mut ctx.accounts.rebalance_state;
    st.bump = ctx.bumps.rebalance_state;
    st.basket = basket_key;
    st.pending = true;
    st.pending_slot = Clock::get()?.slot;
    st.sell_index = SWEEP_INDEX;
    st.buy_index = buy_index;
    st.sell_amount = sell_amount;
    st.min_buy = min_buy;
    st.buy_balance_before = book.rows[b].balance;
    Ok(())
}

/// The oracle floor: the least the vault accepts back for `sell_value` (USD x1e12), in the
/// bought token's units. Transfer fees aren't slippage: the lent tokens reach the rebalancer
/// less the sold token's fee, and the bought ones reach the vault less theirs, so the floor
/// allows for both. No new money goes into a sleeve whose fee is over the cap; selling one out
/// is always allowed. A floor of zero would accept nothing back, so a slice that small is
/// refused.
fn oracle_floor(sell_value: u128, max_slippage_bps: u16, sell_fee_bps: u16, buy_mint: &AccountInfo, buy: &Row) -> Result<u64> {
    let buy_fee = TransferFee::of(buy_mint)?;
    buy_fee.assert_within_cap()?;
    let floor_value = sell_value * (BPS_DENOM - max_slippage_bps as u128) / BPS_DENOM * (BPS_DENOM - sell_fee_bps as u128) / BPS_DENOM
        * (BPS_DENOM - buy_fee.bps as u128)
        / BPS_DENOM;
    let min_buy = u64::try_from(
        floor_value
            .checked_mul(10u128.pow(buy.decimals as u32))
            .ok_or(VaultError::MathOverflow)?
            / buy.price_e12 as u128,
    )
    .map_err(|_| VaultError::MathOverflow)?;
    require!(min_buy > 0, VaultError::SliceTooSmall);
    Ok(min_buy)
}

/// The finish must come later in this very transaction, for this basket.
fn assert_finish_follows(ixs: &AccountInfo, basket: &Pubkey) -> Result<()> {
    let mut i = load_current_index_checked(ixs)? as usize + 1;
    while let Ok(ix) = load_instruction_at_checked(i, ixs) {
        if ix.program_id == crate::ID
            && ix.data.len() >= 8
            && ix.data[..8] == *<crate::instruction::RebalanceFinish as anchor_lang::Discriminator>::DISCRIMINATOR
            && ix.accounts.get(2).map(|a| a.pubkey) == Some(*basket)
        {
            return Ok(());
        }
        i += 1;
    }
    err!(VaultError::FinishMissing)
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct RebalanceStart<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    pub share_mint: InterfaceAccount<'info, MintI>,
    #[account(init_if_needed, payer = rebalancer, space = 8 + RebalanceState::INIT_SPACE, seeds = [REBALANCE_SEED, basket.key().as_ref()], bump)]
    pub rebalance_state: Account<'info, RebalanceState>,
    /// CHECK: must be the sold constituent's mint; checked in the instruction.
    pub sell_mint: UncheckedAccount<'info>,
    /// CHECK: must be the bought constituent's mint (read for its transfer fee); checked in
    /// the instruction.
    pub buy_mint: UncheckedAccount<'info>,
    /// CHECK: the basket's vault account for the sold constituent; checked in the instruction.
    #[account(mut)]
    pub sell_vault: UncheckedAccount<'info>,
    /// CHECK: receives the lent tokens; the token program validates it on transfer.
    #[account(mut)]
    pub rebalancer_sell: UncheckedAccount<'info>,
    /// CHECK: must own `sell_mint`; checked in the instruction.
    pub sell_token_program: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar, to find the matching finish.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RebalanceFinish<'info> {
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    pub share_mint: InterfaceAccount<'info, MintI>,
    #[account(mut, seeds = [REBALANCE_SEED, basket.key().as_ref()], bump = rebalance_state.bump, has_one = basket)]
    pub rebalance_state: Account<'info, RebalanceState>,
}

#[derive(Accounts)]
pub struct SweepStart<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    pub share_mint: InterfaceAccount<'info, MintI>,
    #[account(init_if_needed, payer = rebalancer, space = 8 + RebalanceState::INIT_SPACE, seeds = [REBALANCE_SEED, basket.key().as_ref()], bump)]
    pub rebalance_state: Account<'info, RebalanceState>,
    #[account(mint::token_program = stray_token_program)]
    pub stray_mint: InterfaceAccount<'info, MintI>,
    /// CHECK: the basket's canonical account for the stray token, checked by `read_vault`.
    #[account(mut)]
    pub stray_vault: UncheckedAccount<'info>,
    #[account(seeds = [PRICE_SEED, stray_mint.key().as_ref()], bump = stray_price.bump)]
    pub stray_price: Account<'info, AssetPrice>,
    /// CHECK: must be the bought constituent's mint (read for its transfer fee); checked in
    /// the instruction.
    pub buy_mint: UncheckedAccount<'info>,
    /// CHECK: receives the lent tokens; the token program validates it on transfer.
    #[account(mut)]
    pub rebalancer_sell: UncheckedAccount<'info>,
    pub stray_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the instructions sysvar, to find the matching finish.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}
