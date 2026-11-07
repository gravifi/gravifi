//! GRAVI staking: any stake shares stakers' 10% of protocol fees, in SOL, in proportion; and each
//! whole threshold staked (1M GRAVI) makes one basket a day with no GRAVI burned.
//!
//! The 10% is counted on the basket at each mint and redeem and stays in the treasury's shares
//! until `stake_sweep` moves it to the rewards PDA, which redeems and sells it for SOL as the
//! buyback does. `stake_fund` turns the SOL into a pot paid out per second over a month, in
//! proportion to each stake that counts, and `stake_claim` pays what a stake has earned.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::instructions::buyback::{finish_follows, oracle_floor, redeem_into};
use crate::state::*;
use crate::token_util::*;

/// Sets a protocol flag, growing the protocol account's tail to hold it the first time.
pub(crate) fn set_protocol_flag<'info>(protocol: &AccountInfo<'info>, payer: &AccountInfo<'info>, system_program: &AccountInfo<'info>, flag: u8, on: bool) -> Result<()> {
    let need = PROTOCOL_FLAGS_OFFSET + 1;
    if protocol.data_len() < need {
        let short = Rent::get()?.minimum_balance(need).saturating_sub(protocol.lamports());
        if short > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(system_program.clone(), anchor_lang::system_program::Transfer { from: payer.clone(), to: protocol.clone() }),
                short,
            )?;
        }
        protocol.resize(need)?;
    }
    let mut data = protocol.try_borrow_mut_data()?;
    data[PROTOCOL_FLAGS_OFFSET] = if on { data[PROTOCOL_FLAGS_OFFSET] | flag } else { data[PROTOCOL_FLAGS_OFFSET] & !flag };
    Ok(())
}

/// Sets up the stake pool and turns staking on: from here every protocol fee owes stakers their
/// share, for good. Protocol authority, once.
pub fn init_staking(ctx: Context<InitStaking>) -> Result<()> {
    let a = &mut *ctx.accounts;
    let one_gravi = 10u64.checked_pow(a.gravi_mint.decimals as u32).ok_or(VaultError::MathOverflow)?;
    let p = &mut a.pool;
    p.bump = ctx.bumps.pool;
    p.gravi_mint = a.gravi_mint.key();
    p.one_gravi = one_gravi;
    p.threshold = STAKE_THRESHOLD_TOKENS.checked_mul(one_gravi).ok_or(VaultError::MathOverflow)?;
    set_protocol_flag(&a.protocol.to_account_info(), &a.authority.to_account_info(), &a.system_program.to_account_info(), FLAG_STAKING, true)
}

/// Moves the threshold, in GRAVI base units, up to its ceiling. Lower applies now; higher after
/// STAKE_THRESHOLD_NOTICE_SECS. Either replaces a raise still waiting. Protocol authority.
pub fn set_stake_threshold(ctx: Context<SetStakeThreshold>, threshold: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let p = &mut ctx.accounts.pool;
    let ceiling = MAX_STAKE_THRESHOLD_TOKENS.checked_mul(p.one_gravi).ok_or(VaultError::MathOverflow)?;
    require!(threshold > 0 && threshold <= ceiling, VaultError::BadStakeThreshold);
    p.threshold = p.threshold_at(now);
    p.next_threshold_at = 0;
    if threshold <= p.threshold {
        p.threshold = threshold;
    } else {
        p.next_threshold = threshold;
        p.next_threshold_at = now + STAKE_THRESHOLD_NOTICE_SECS;
    }
    Ok(())
}

/// Stakes `amount` GRAVI, any amount. It counts STAKE_WARMUP_SECS from now; a top-up waits on its own,
/// and what already counts keeps counting.
pub fn stake(ctx: Context<StakeMove>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::BadParameter);
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    let before = read_vault(&a.pool_gravi.to_account_info(), &a.pool.key(), &a.gravi_mint.key())?.amount;
    token_interface::transfer_checked(
        CpiContext::new(
            a.gravi_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.wallet_gravi.to_account_info(),
                mint: a.gravi_mint.to_account_info(),
                to: a.pool_gravi.to_account_info(),
                authority: a.wallet.to_account_info(),
            },
        ),
        amount,
        a.gravi_mint.decimals,
    )?;
    let received = read_vault(&a.pool_gravi.to_account_info(), &a.pool.key(), &a.gravi_mint.key())?.amount.saturating_sub(before);

    a.pool.accrue(now)?;
    let s = &mut a.stake;
    s.bump = ctx.bumps.stake;
    s.wallet = a.wallet.key();
    s.settle(&a.pool)?;
    s.sync(&mut a.pool, now)?;
    s.amount = s.amount.checked_add(received).ok_or(VaultError::MathOverflow)?;
    s.pending = s.pending.checked_add(received).ok_or(VaultError::MathOverflow)?;
    s.pending_at = now;
    s.sync(&mut a.pool, now)
}

/// Starts `amount` on its way out, what's still warming first: it earns and frees nothing from now
/// on, and can be withdrawn after STAKE_UNSTAKE_SECS. Asking again restarts the wait for all of it.
pub fn unstake(ctx: Context<StakeUpdate>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::BadParameter);
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    require!(amount <= a.stake.amount, VaultError::NotEnoughStaked);
    a.pool.accrue(now)?;
    let s = &mut a.stake;
    s.settle(&a.pool)?;
    s.sync(&mut a.pool, now)?;
    s.pending -= s.pending.min(amount);
    s.amount -= amount;
    s.unstaking = s.unstaking.checked_add(amount).ok_or(VaultError::MathOverflow)?;
    s.unstake_at = now + STAKE_UNSTAKE_SECS;
    s.sync(&mut a.pool, now)
}

/// Pays out GRAVI that has waited out its days.
pub fn stake_withdraw(ctx: Context<StakeWithdraw>) -> Result<()> {
    let a = &mut *ctx.accounts;
    let amount = a.stake.unstaking;
    require!(amount > 0, VaultError::NothingToTake);
    require!(Clock::get()?.unix_timestamp >= a.stake.unstake_at, VaultError::UnstakeWaiting);
    a.stake.unstaking = 0;
    read_vault(&a.pool_gravi.to_account_info(), &a.pool.key(), &a.gravi_mint.key())?;
    let bump = [a.pool.bump];
    let seeds: [&[u8]; 2] = [STAKE_POOL_SEED, &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            a.gravi_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.pool_gravi.to_account_info(),
                mint: a.gravi_mint.to_account_info(),
                to: a.wallet_gravi.to_account_info(),
                authority: a.pool.to_account_info(),
            },
            &[&seeds],
        ),
        amount,
        a.gravi_mint.decimals,
    )
}

/// Pays a stake what it has earned, as wrapped SOL to the wallet's own SOL account. The app closes
/// that account in the same transaction, which unwraps it.
pub fn stake_claim(ctx: Context<StakeClaim>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    a.pool.accrue(now)?;
    a.stake.settle(&a.pool)?;
    let owed = a.stake.owed;
    require!(owed > 0, VaultError::NothingToTake);
    a.stake.owed = 0;
    a.pool.claimed = a.pool.claimed.checked_add(owed).ok_or(VaultError::MathOverflow)?;
    read_vault(&a.pot.to_account_info(), &a.rewards.key(), &NATIVE_MINT)?;
    let bump = [ctx.bumps.rewards];
    let seeds: [&[u8]; 2] = [STAKE_REWARDS_SEED, &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            a.token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.pot.to_account_info(),
                mint: a.native_mint.to_account_info(),
                to: a.wallet_sol.to_account_info(),
                authority: a.rewards.to_account_info(),
            },
            &[&seeds],
        ),
        owed,
        a.native_mint.decimals,
    )
}

/// Closes an empty, paid-out stake, its rent back to the wallet.
pub fn stake_close(ctx: Context<StakeClose>) -> Result<()> {
    let s = &ctx.accounts.stake;
    require!(s.amount == 0 && s.unstaking == 0 && s.owed == 0, VaultError::StakeNotEmpty);
    Ok(())
}

/// Books what of a stake has warmed up, so it counts toward the pot. Permissionless; the keeper runs
/// it.
pub fn stake_refresh(ctx: Context<StakeRefresh>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    a.pool.accrue(now)?;
    a.stake.settle(&a.pool)?;
    a.stake.sync(&mut a.pool, now)
}

/// Starts the next month's pot from whatever SOL the rewards PDA holds beyond what's already
/// promised, once the last month's has paid out. Anyone may call it, and anyone may add SOL to the
/// pot first by sending wrapped SOL to the account.
pub fn stake_fund(ctx: Context<StakeFund>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    require!(now >= a.pool.stream_end, VaultError::StreamRunning);
    a.pool.accrue(now)?;
    let held = read_vault(&a.pot.to_account_info(), &a.rewards.key(), &NATIVE_MINT)?.amount;
    let p = &mut a.pool;
    let promised = p.funded.saturating_sub(p.claimed);
    let new = held.saturating_sub(promised);
    require!(new > 0, VaultError::NothingToTake);
    p.rate = new as u128 * STAKE_RATE_SCALE / STAKE_PERIOD_SECS as u128;
    p.last_update = now;
    p.stream_end = now + STAKE_PERIOD_SECS;
    p.funded = p.funded.checked_add(new).ok_or(VaultError::MathOverflow)?;
    p.periods = p.periods.saturating_add(1);
    Ok(())
}

/// Moves the shares a basket owes stakers out of its treasury shares, to the rewards PDA.
/// Permissionless.
pub fn stake_sweep(ctx: Context<StakeSweep>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(a.basket.control != ControlMode::Dao, VaultError::BadParameter);
    let held = read_vault(&a.treasury_shares.to_account_info(), &a.protocol.key(), &a.share_mint.key())?.amount;
    read_vault(&a.reward_shares.to_account_info(), &a.rewards.key(), &a.share_mint.key())?;
    // What the treasury holds for the creator isn't stakers' to take.
    let amount = a.basket.stake_owed.min(held.saturating_sub(a.basket.owed_to_creator()));
    require!(amount > 0, VaultError::NothingToTake);
    a.basket.stake_owed -= amount;
    let bump = [a.protocol.bump];
    let seeds: [&[u8]; 2] = [PROTOCOL_SEED, &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            a.share_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.treasury_shares.to_account_info(),
                mint: a.share_mint.to_account_info(),
                to: a.reward_shares.to_account_info(),
                authority: a.protocol.to_account_info(),
            },
            &[&seeds],
        ),
        amount,
        a.share_mint.decimals,
    )
}

/// Turns swept shares of one basket into its tokens, held by the rewards PDA, as `buyback_redeem`
/// does for the burn PDA. Permissionless. Remaining accounts: one `[vault_ata, rewards_ata, mint,
/// token_program]` quad per constituent.
pub fn stake_redeem<'info>(ctx: Context<'_, '_, 'info, 'info, StakeRedeem<'info>>, shares: u64) -> Result<()> {
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
    require!(shares > 0, VaultError::ZeroShares);
    // Never from inside another program: a sale's finish would count what this delivers as what
    // the sale brought back.
    require!(
        anchor_lang::solana_program::instruction::get_stack_height() == anchor_lang::solana_program::instruction::TRANSACTION_LEVEL_STACK_HEIGHT,
        VaultError::BuybackInterleaved
    );
    let bump = [ctx.bumps.rewards];
    redeem_into(
        &mut ctx.accounts.basket,
        &ctx.accounts.share_mint,
        &ctx.accounts.reward_shares,
        ctx.accounts.rewards.as_ref(),
        &[STAKE_REWARDS_SEED, &bump],
        &ctx.accounts.share_token_program,
        ctx.remaining_accounts,
        shares,
    )
}

/// First half of a sale to SOL, as `buyback_sol_start`: lends a token the rewards PDA holds to the
/// rebalancer, who delivers wrapped SOL to the pot in the same transaction. `stake_sell_finish`
/// must follow and checks at least the oracle value, less `max_slippage_bps`, came back.
pub fn stake_sell_start(ctx: Context<StakeSellStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
    require!(sell_amount > 0, VaultError::BadParameter);
    require!(max_slippage_bps <= MAX_BUYBACK_SLIPPAGE_BPS, VaultError::BadParameter);
    let a = &mut *ctx.accounts;
    require!(!a.pool.sale_pending, VaultError::BuybackPending);
    let sell_mint = a.sell_mint.to_account_info();
    let min_sol = oracle_floor(sell_amount, &a.sell_price, &a.sol_price, max_slippage_bps, refund, &sell_mint)?;
    finish_follows(&a.instructions, <crate::instruction::StakeSellFinish as anchor_lang::Discriminator>::DISCRIMINATOR)?;
    let bump = [ctx.bumps.rewards];
    let seeds: [&[u8]; 2] = [STAKE_REWARDS_SEED, &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            a.sell_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.sell_source.to_account_info(),
                mint: sell_mint,
                to: a.rebalancer_sell.to_account_info(),
                authority: a.rewards.to_account_info(),
            },
            &[&seeds],
        ),
        sell_amount.checked_add(refund).ok_or(VaultError::MathOverflow)?,
        a.sell_mint.decimals,
    )?;
    let held = read_vault(&a.pot.to_account_info(), &a.rewards.key(), &NATIVE_MINT)?.amount;
    let p = &mut a.pool;
    p.sale_pending = true;
    p.sale_slot = Clock::get()?.slot;
    p.sale_min_sol = min_sol;
    p.sale_sol_before = held;
    Ok(())
}

/// Second half of a sale to SOL: checks the SOL that came back clears the oracle floor.
pub fn stake_sell_finish(ctx: Context<StakeSellFinish>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(a.pool.sale_pending && a.pool.sale_slot == Clock::get()?.slot, VaultError::NoBuybackPending);
    let held = read_vault(&a.pot.to_account_info(), &a.rewards.key(), &NATIVE_MINT)?.amount;
    let got = held.saturating_sub(a.pool.sale_sol_before);
    require!(got >= a.pool.sale_min_sol, VaultError::BelowOracleFloor);
    a.pool.sale_pending = false;
    a.pool.total_sol = a.pool.total_sol.checked_add(got).ok_or(VaultError::MathOverflow)?;
    Ok(())
}

/// Closes an empty token account the rewards PDA owns, its rent back to the rebalancer who opened
/// it, so a month's sales cost only network fees. Never the pot.
pub fn stake_close_account(ctx: Context<StakeCloseAccount>) -> Result<()> {
    require!(ctx.accounts.account.amount == 0, VaultError::BurnAccountNotEmpty);
    let bump = [ctx.bumps.rewards];
    let seeds: [&[u8]; 2] = [STAKE_REWARDS_SEED, &bump];
    token_interface::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        token_interface::CloseAccount {
            account: ctx.accounts.account.to_account_info(),
            destination: ctx.accounts.rebalancer.to_account_info(),
            authority: ctx.accounts.rewards.to_account_info(),
        },
        &[&seeds],
    ))
}

/// What a free basket costs a stake: one of the day's (a creation, an added asset), or nothing but
/// having one a day (making a basket smart while it's being created).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreeUse {
    TakeOne,
    HaveOne,
}

/// Who burns no GRAVI here, when `due` would be burned: `owner`, if its stake (writable) and the pool
/// follow and it has a free basket left today, which this then uses up; otherwise the burn-exempt
/// wallet. Nothing due, or the exempt wallet, uses no free basket. A stake with none left just burns.
pub(crate) fn burn_skip(owner: &Pubkey, extra: &[AccountInfo], protocol: &AccountInfo, due: u64, use_: FreeUse) -> Result<Option<Pubkey>> {
    let exempt = burn_exempt(protocol);
    if due == 0 || exempt == Some(*owner) {
        return Ok(exempt);
    }
    if let [stake_ai, pool_ai] = extra {
        require_keys_eq!(*stake_ai.owner, crate::ID, VaultError::BadStake);
        require_keys_eq!(*pool_ai.owner, crate::ID, VaultError::BadStake);
        let mut stake = Stake::try_deserialize(&mut &stake_ai.try_borrow_data()?[..])?;
        let pool = StakePool::try_deserialize(&mut &pool_ai.try_borrow_data()?[..])?;
        require_keys_eq!(stake.wallet, *owner, VaultError::BadStake);
        let now = Clock::get()?.unix_timestamp;
        let threshold = pool.threshold_at(now);
        let free = match use_ {
            FreeUse::HaveOne => stake.free_per_day(threshold, now) > 0,
            FreeUse::TakeOne => {
                require!(stake_ai.is_writable, VaultError::BadStake);
                let took = stake.take_free(threshold, now);
                if took {
                    let mut data = stake_ai.try_borrow_mut_data()?;
                    let mut out: &mut [u8] = &mut data;
                    stake.try_serialize(&mut out)?;
                }
                took
            }
        };
        if free {
            return Ok(Some(*owner));
        }
    }
    Ok(exempt)
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct InitStaking<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// Grown by hand to hold the flags after its fields (see PROTOCOL_FLAGS_OFFSET).
    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Box<Account<'info, Protocol>>,
    #[account(address = protocol.gravi_mint @ VaultError::GraviMintNotSet)]
    pub gravi_mint: Box<InterfaceAccount<'info, MintI>>,
    #[account(init, payer = authority, space = 8 + StakePool::INIT_SPACE, seeds = [STAKE_POOL_SEED], bump)]
    pub pool: Box<Account<'info, StakePool>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetStakeThreshold<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Box<Account<'info, Protocol>>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
}

#[derive(Accounts)]
pub struct StakeMove<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    #[account(init_if_needed, payer = wallet, space = 8 + Stake::INIT_SPACE, seeds = [STAKE_SEED, wallet.key().as_ref()], bump)]
    pub stake: Box<Account<'info, Stake>>,
    #[account(address = pool.gravi_mint)]
    pub gravi_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: the wallet's GRAVI account; the token program checks it as it moves.
    #[account(mut)]
    pub wallet_gravi: UncheckedAccount<'info>,
    /// CHECK: the pool's canonical GRAVI account, checked in the instruction.
    #[account(mut)]
    pub pool_gravi: UncheckedAccount<'info>,
    pub gravi_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct StakeUpdate<'info> {
    pub wallet: Signer<'info>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, wallet.key().as_ref()], bump = stake.bump)]
    pub stake: Box<Account<'info, Stake>>,
}

#[derive(Accounts)]
pub struct StakeWithdraw<'info> {
    pub wallet: Signer<'info>,
    #[account(seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, wallet.key().as_ref()], bump = stake.bump)]
    pub stake: Box<Account<'info, Stake>>,
    #[account(address = pool.gravi_mint)]
    pub gravi_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: where the wallet takes its GRAVI; the token program checks the mint.
    #[account(mut)]
    pub wallet_gravi: UncheckedAccount<'info>,
    /// CHECK: the pool's canonical GRAVI account, checked in the instruction.
    #[account(mut)]
    pub pool_gravi: UncheckedAccount<'info>,
    pub gravi_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct StakeClaim<'info> {
    pub wallet: Signer<'info>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, wallet.key().as_ref()], bump = stake.bump)]
    pub stake: Box<Account<'info, Stake>>,
    /// CHECK: address-only PDA; signs as the owner of the pot.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    #[account(address = NATIVE_MINT)]
    pub native_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: the rewards PDA's canonical SOL account, checked in the instruction.
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: where the wallet takes its SOL; the token program checks the mint.
    #[account(mut)]
    pub wallet_sol: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct StakeClose<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, close = wallet, seeds = [STAKE_SEED, wallet.key().as_ref()], bump = stake.bump)]
    pub stake: Box<Account<'info, Stake>>,
}

#[derive(Accounts)]
pub struct StakeRefresh<'info> {
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, stake.wallet.as_ref()], bump = stake.bump)]
    pub stake: Box<Account<'info, Stake>>,
}

#[derive(Accounts)]
pub struct StakeFund<'info> {
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    /// CHECK: address-only PDA, the owner of the pot.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    /// CHECK: the rewards PDA's canonical SOL account, checked in the instruction.
    pub pot: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct StakeSweep<'info> {
    #[account(mut, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    /// CHECK: the treasury's canonical share account, checked in the instruction.
    #[account(mut)]
    pub treasury_shares: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the owner of swept shares.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    /// CHECK: the rewards PDA's canonical share account, checked in the instruction.
    #[account(mut)]
    pub reward_shares: UncheckedAccount<'info>,
    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct StakeRedeem<'info> {
    #[account(has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,
    /// CHECK: address-only PDA; signs as the owner of swept shares.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    #[account(mut, token::mint = share_mint, token::authority = rewards)]
    pub reward_shares: InterfaceAccount<'info, TokenAccountI>,
    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct StakeSellStart<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Box<Account<'info, Keepers>>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    /// CHECK: address-only PDA; signs as the owner of what's sold.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    #[account(mint::token_program = sell_token_program, constraint = sell_mint.key() != NATIVE_MINT @ VaultError::BadParameter)]
    pub sell_mint: Box<InterfaceAccount<'info, MintI>>,
    #[account(mut, token::mint = sell_mint, token::authority = rewards, token::token_program = sell_token_program)]
    pub sell_source: Box<InterfaceAccount<'info, TokenAccountI>>,
    /// CHECK: receives the lent tokens; the token program validates it on transfer.
    #[account(mut)]
    pub rebalancer_sell: UncheckedAccount<'info>,
    #[account(seeds = [PRICE_SEED, sell_mint.key().as_ref()], bump = sell_price.bump)]
    pub sell_price: Box<Account<'info, AssetPrice>>,
    #[account(seeds = [PRICE_SEED, NATIVE_MINT.as_ref()], bump = sol_price.bump)]
    pub sol_price: Box<Account<'info, AssetPrice>>,
    /// CHECK: the rewards PDA's canonical SOL account, checked in the instruction.
    pub pot: UncheckedAccount<'info>,
    pub sell_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the instructions sysvar, to find the matching finish.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct StakeSellFinish<'info> {
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Box<Account<'info, Keepers>>,
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = pool.bump)]
    pub pool: Box<Account<'info, StakePool>>,
    /// CHECK: address-only PDA, the owner of the pot.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    /// CHECK: the rewards PDA's canonical SOL account, checked in the instruction.
    pub pot: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct StakeCloseAccount<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Box<Account<'info, Keepers>>,
    /// CHECK: address-only PDA; signs as the account's owner.
    #[account(seeds = [STAKE_REWARDS_SEED], bump)]
    pub rewards: UncheckedAccount<'info>,
    #[account(mint::token_program = token_program, constraint = mint.key() != NATIVE_MINT @ VaultError::BurnAccountKept)]
    pub mint: Box<InterfaceAccount<'info, MintI>>,
    #[account(mut, token::mint = mint, token::authority = rewards, token::token_program = token_program)]
    pub account: Box<InterfaceAccount<'info, TokenAccountI>>,
    pub token_program: Interface<'info, TokenInterface>,
}
