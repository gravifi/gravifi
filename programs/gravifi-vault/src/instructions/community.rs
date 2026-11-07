//! Share staking and community baskets. Any basket's holders can stake its shares; in a community
//! basket, staked shares vote on proposals: what it holds, its policy, its stops, its splits, a
//! lower creator fee, handing it to the strategist, or leaving Community. A proposal takes a
//! 3-day vote and a 24-hour wait, then anyone can execute it, and the program holds every change
//! to the same limits a smart basket's strategist is held to.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hashv;
use anchor_spl::token_interface::{self, Mint as MintI, TokenInterface};

use crate::errors::VaultError;
use crate::instructions::basket::{apply_split, burn_gravi, check_addable, check_policy, drop_emptied, GraviAccounts};
use crate::instructions::smart::{check_guardrails, is_verified};
use crate::instructions::staking::{burn_skip, set_protocol_flag, FreeUse};
use crate::instructions::stops::{check_stops, hold_to_price, write_stops};
use crate::state::*;
use crate::token_util::*;

/// Opens or closes the door to Community mode. Baskets already in carry on. Protocol authority.
pub fn set_community_open(ctx: Context<SetCommunityOpen>, open: bool) -> Result<()> {
    let a = &ctx.accounts;
    set_protocol_flag(&a.protocol.to_account_info(), &a.authority.to_account_info(), &a.system_program.to_account_info(), FLAG_COMMUNITY, open)
}

/// Stakes `amount` of a basket's shares in its escrow. Weight builds over VOTE_RAMP_SECS.
pub fn share_stake(ctx: Context<ShareStakeIn>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::BadParameter);
    let a = &mut *ctx.accounts;
    read_vault(&a.escrow.to_account_info(), &a.escrow_authority.key(), &a.share_mint.key())?;
    token_interface::transfer_checked(
        CpiContext::new(
            a.share_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.wallet_shares.to_account_info(),
                mint: a.share_mint.to_account_info(),
                to: a.escrow.to_account_info(),
                authority: a.wallet.to_account_info(),
            },
        ),
        amount,
        a.share_mint.decimals,
    )?;
    let s = &mut a.share_stake;
    s.bump = ctx.bumps.share_stake;
    s.basket = a.basket.key();
    s.wallet = a.wallet.key();
    s.add(amount, Clock::get()?.unix_timestamp)
}

/// Takes staked shares back, unless they voted in a vote still running. The account closes when
/// it empties, its rent back to the wallet.
pub fn share_unstake(ctx: Context<ShareStakeOut>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::BadParameter);
    let a = &mut *ctx.accounts;
    require!(amount <= a.share_stake.amount, VaultError::NotEnoughStaked);
    require!(Clock::get()?.unix_timestamp >= a.share_stake.locked_until, VaultError::SharesLocked);
    read_vault(&a.escrow.to_account_info(), &a.escrow_authority.key(), &a.share_mint.key())?;
    let bump = [ctx.bumps.escrow_authority];
    let seeds: [&[u8]; 2] = [SHARE_ESCROW_SEED, &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            a.share_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: a.escrow.to_account_info(),
                mint: a.share_mint.to_account_info(),
                to: a.wallet_shares.to_account_info(),
                authority: a.escrow_authority.to_account_info(),
            },
            &[&seeds],
        ),
        amount,
        a.share_mint.decimals,
    )?;
    a.share_stake.amount -= amount;
    if a.share_stake.amount == 0 {
        return a.share_stake.close(a.wallet.to_account_info());
    }
    Ok(())
}

/// Hands a seeded, public, creator-managed basket to its holders. One way: going back takes a
/// vote. The creator pays the voting state's rent.
pub fn enter_community(ctx: Context<EnterCommunity>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(protocol_flags(&a.protocol.to_account_info()) & FLAG_COMMUNITY != 0, VaultError::CommunityClosed);
    let b = &mut a.basket;
    require!(b.control == ControlMode::Creator && b.seeded && b.access == AccessMode::Public, VaultError::CommunityNotAllowed);
    b.control = ControlMode::Community;
    b.community = 1;
    b.set_vote_added_at(0);
    let c = &mut a.community;
    c.bump = ctx.bumps.community;
    c.basket = b.key();
    c.hour = Clock::get()?.unix_timestamp / 3_600;
    c.turnover = [0; TURNOVER_HOURS];
    Ok(())
}

/// What pins a Reweight's targets to the basket's assets.
fn mints_hash(basket: &Basket) -> [u8; 32] {
    let mints: Vec<&[u8]> = basket.active().iter().map(|c| c.mint.as_ref()).collect();
    hashv(&mints).to_bytes()
}

/// Whether this basket, as it stands, can take this change. Run on proposing, so nobody votes on
/// one that can't happen; the execute checks what may have changed since.
fn check_action(basket: &Basket, action: &ProposalAction) -> Result<()> {
    let voting = basket.control == ControlMode::Community;
    match action {
        ProposalAction::Reweight { target_bps, mints_hash: pinned } => {
            require!(voting && target_bps.len() == basket.constituent_count as usize, VaultError::BadProposal);
            require!(*pinned == mints_hash(basket), VaultError::AssetsChanged);
            let sum: u32 = target_bps.iter().map(|b| *b as u32).sum();
            require!(sum == BPS_DENOM as u32, VaultError::WeightsNotNormalised);
            require!(target_bps.iter().filter(|t| **t > 0).count() >= 2, VaultError::AssetCountOutOfRange);
            let change: u32 = target_bps.iter().zip(basket.active()).map(|(new, c)| new.saturating_sub(c.target_bps) as u32).sum();
            require!(change <= SMART_MAX_DAY_BPS as u32, VaultError::TurnoverDayCap);
        }
        ProposalAction::AddAsset { mint } => {
            require!(voting && (basket.constituent_count as usize) < MAX_LAUNCH_CONSTITUENTS, VaultError::BadProposal);
            require!(basket.active().iter().all(|c| c.mint != *mint), VaultError::DuplicateConstituent);
        }
        ProposalAction::Policy { rebalance_mode, interval_secs, drift_bps } => check_policy(*rebalance_mode, *interval_secs, *drift_bps)?,
        ProposalAction::Stops { stops } => check_stops(basket, stops)?,
        ProposalAction::CreatorFee { mgmt_fee_bps } => require!(*mgmt_fee_bps < basket.mgmt_fee_bps, VaultError::BadProposal),
        ProposalAction::CreatorCut { bps } => require!(*bps < basket.cut_bps(), VaultError::BadProposal),
        ProposalAction::Split { numerator, denominator } => {
            require!((1..=100).contains(numerator) && (1..=100).contains(denominator) && numerator != denominator, VaultError::BadProposal)
        }
        ProposalAction::GoSmart { mandate_hash, guard } => {
            require!(voting, VaultError::BadProposal);
            check_guardrails(guard, mandate_hash)?;
        }
        ProposalAction::Mandate { mandate_hash, guard } => {
            require!(!voting, VaultError::BadProposal);
            check_guardrails(guard, mandate_hash)?;
        }
        ProposalAction::Leave => {}
    }
    Ok(())
}

/// Opens a proposal. The proposer's staked shares must carry 1% of the supply right now, and they
/// may have one open proposal on the basket at a time (`OpenProposal`, whose rent they pay and get
/// back at `community_close`): keeping all MAX_OPEN_PROPOSALS slots full takes three wallets with
/// 1% each, not one. Adding an asset or going Smart burns what the creator would have, from the
/// proposer, unless their GRAVI stake has a free basket left today, which this uses (its accounts
/// follow in the remaining accounts); it's burned win or lose.
pub fn community_propose<'info>(ctx: Context<'_, '_, 'info, 'info, CommunityPropose<'info>>, action: ProposalAction) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    require!(a.basket.by_vote(), VaultError::NotCommunity);
    require!(a.community.open < MAX_OPEN_PROPOSALS, VaultError::TooManyProposals);
    check_action(&a.basket, &action)?;
    let supply = a.share_mint.supply;
    let need = (supply as u128 * PROPOSE_MIN_BPS).div_ceil(BPS_DENOM);
    require!(a.share_stake.power_at(now)? as u128 >= need, VaultError::BelowProposalWeight);
    // Created here, or still open from an earlier proposal of theirs.
    require!(a.open_proposal.basket == Pubkey::default(), VaultError::ProposerHasOpenProposal);

    let protocol = a.protocol.to_account_info();
    let burn = match action {
        ProposalAction::AddAsset { .. } => a.creation_burn.per_asset,
        ProposalAction::GoSmart { .. } => smart_burn(&protocol),
        _ => 0,
    };
    burn_gravi(
        &mut a.creation_burn,
        &a.protocol,
        GraviAccounts { mint: a.gravi_mint.as_ref(), from: a.proposer_gravi.as_ref(), program: a.gravi_token_program.as_ref() },
        a.proposer.to_account_info(),
        burn,
        false,
        burn_skip(&a.proposer.key(), ctx.remaining_accounts, &protocol, burn, FreeUse::TakeOne)?,
    )?;

    let p = &mut a.proposal;
    p.bump = ctx.bumps.proposal;
    p.basket = a.basket.key();
    p.id = a.community.next_id;
    p.epoch = a.community.epoch;
    p.proposer = a.proposer.key();
    p.created_at = now;
    p.supply = supply;
    p.votes_for = 0;
    p.votes_against = 0;
    p.executed = false;
    p.action = action;
    let o = &mut a.open_proposal;
    o.bump = ctx.bumps.open_proposal;
    o.basket = a.basket.key();
    o.id = p.id;
    let c = &mut a.community;
    c.next_id = c.next_id.checked_add(1).ok_or(VaultError::MathOverflow)?;
    c.open += 1;
    Ok(())
}

/// Votes all a wallet's weight as of the proposal's opening. Its shares stay staked until the vote
/// ends, and it can vote once.
pub fn community_vote(ctx: Context<CommunityVote>, support: bool) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let a = &mut *ctx.accounts;
    let end = a.proposal.vote_end();
    require!(now < end, VaultError::VotingOver);
    let power = a.share_stake.power_at(a.proposal.created_at)?;
    require!(power > 0, VaultError::NoVotingWeight);
    let p = &mut a.proposal;
    let tally = if support { &mut p.votes_for } else { &mut p.votes_against };
    *tally = tally.checked_add(power).ok_or(VaultError::MathOverflow)?;
    a.share_stake.locked_until = a.share_stake.locked_until.max(end);
    let v = &mut a.vote;
    v.bump = ctx.bumps.vote;
    v.wallet = a.wallet.key();
    v.vote_end = end;
    v.support = support;
    Ok(())
}

/// Checks a proposal may run now and marks it run.
fn take_ready(basket: &Basket, community: &Community, proposal: &mut Proposal) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(basket.by_vote(), VaultError::NotCommunity);
    require!(
        proposal.epoch == community.epoch && !proposal.executed && now >= proposal.exec_after() && now <= proposal.exec_until() && proposal.passed(),
        VaultError::NotExecutable
    );
    proposal.executed = true;
    Ok(())
}

/// Hands a basket back to its creator.
fn leave(basket: &mut Basket, community: &mut Community) -> Result<()> {
    basket.control = ControlMode::Creator;
    basket.community = 0;
    community.epoch = community.epoch.checked_add(1).ok_or(VaultError::MathOverflow)?;
    Ok(())
}

/// Runs a passed Reweight, Policy, CreatorFee, CreatorCut, or Leave of a basket not handed to the strategist.
/// Permissionless. For a Reweight, remaining accounts: each asset's Verified record address, in
/// order. A CreatorFee needs `accrue_fees` earlier in the same transaction (the creator's fee
/// accrued this second), as the creator's app runs it before `set_policy`: what the old fee
/// streamed would otherwise be forfeit, and anyone can execute.
pub fn community_execute<'info>(ctx: Context<'_, '_, 'info, 'info, CommunityExecute<'info>>) -> Result<()> {
    let a = &mut *ctx.accounts;
    take_ready(&a.basket, &a.community, &mut a.proposal)?;
    match &a.proposal.action {
        ProposalAction::Reweight { target_bps, mints_hash: pinned } => {
            let b = &mut a.basket;
            require!(b.control == ControlMode::Community, VaultError::NotExecutable);
            require!(*pinned == mints_hash(b), VaultError::AssetsChanged);
            let n = b.constituent_count as usize;
            require!(ctx.remaining_accounts.len() == n, VaultError::BadRemainingAccounts);
            let mut change: u32 = 0;
            for i in 0..n {
                let (old, new) = (b.constituents[i].target_bps, target_bps[i]);
                let cap = if is_verified(&ctx.remaining_accounts[i], &b.constituents[i].mint)? { SMART_MAX_WEIGHT_BPS } else { SMART_MAX_UNVERIFIED_BPS };
                require!(new <= cap || new <= old, VaultError::WeightAboveCap);
                change += new.saturating_sub(old) as u32;
            }
            let c: &mut Community = &mut a.community;
            advance_window(&mut c.hour, &mut c.turnover, Clock::get()?.unix_timestamp / 3_600);
            let (day, week) = window_used(c.hour, &c.turnover);
            require!(day + change <= SMART_MAX_DAY_BPS as u32, VaultError::TurnoverDayCap);
            require!(week + change <= SMART_MAX_WEEK_BPS as u32, VaultError::TurnoverWeekCap);
            let k = c.hour.rem_euclid(TURNOVER_HOURS as i64) as usize;
            c.turnover[k] += change as u16;
            c.changes = c.changes.saturating_add(1);
            for i in 0..n {
                b.constituents[i].target_bps = target_bps[i];
            }
        }
        ProposalAction::Policy { rebalance_mode, interval_secs, drift_bps } => {
            check_policy(*rebalance_mode, *interval_secs, *drift_bps)?;
            let b = &mut a.basket;
            b.rebalance_mode = *rebalance_mode;
            b.interval_secs = *interval_secs;
            b.drift_bps = *drift_bps;
        }
        ProposalAction::CreatorFee { mgmt_fee_bps } => {
            let b = &mut a.basket;
            require!(*mgmt_fee_bps < b.mgmt_fee_bps, VaultError::BadProposal);
            let now = Clock::get()?.unix_timestamp;
            require!(b.last_fee_accrual == now, VaultError::FeeNotAccrued);
            b.mgmt_fee_bps = *mgmt_fee_bps;
            b.last_fee_accrual = now;
        }
        ProposalAction::CreatorCut { bps } => {
            let b = &mut a.basket;
            require!(*bps < b.cut_bps(), VaultError::BadProposal);
            b.creator_cut_bps = *bps;
        }
        ProposalAction::Leave => {
            require!(a.basket.control == ControlMode::Community, VaultError::NotExecutable);
            leave(&mut a.basket, &mut a.community)?;
        }
        _ => return err!(VaultError::BadProposal),
    }
    Ok(())
}

/// Runs a passed AddAsset, at 0% like any added asset: a Reweight then gives it a weight. The
/// basket's vault for it must exist. Permissionless. Nothing can be removed from the basket for
/// REMOVE_GRACE_SECS after (`community_remove`), so the Reweight has time to run.
pub fn community_execute_add(ctx: Context<CommunityExecuteAdd>) -> Result<()> {
    let a = &mut *ctx.accounts;
    take_ready(&a.basket, &a.community, &mut a.proposal)?;
    let ProposalAction::AddAsset { mint } = a.proposal.action else { return err!(VaultError::BadProposal) };
    require!(a.basket.control == ControlMode::Community, VaultError::NotExecutable);
    require_keys_eq!(a.mint.key(), mint, VaultError::ConstituentMismatch);
    check_addable(&a.basket, &a.mint.to_account_info(), &a.asset_entry.to_account_info(), &a.vault.to_account_info())?;
    a.basket.constituents.push(Constituent { mint, target_bps: 0, units_per_share: 0 });
    a.basket.constituent_count += 1;
    a.basket.set_vote_added_at(Clock::get()?.unix_timestamp);
    Ok(())
}

/// Runs a passed Split. Permissionless.
pub fn community_execute_split(ctx: Context<CommunityExecuteSplit>) -> Result<()> {
    let a = &mut *ctx.accounts;
    take_ready(&a.basket, &a.community, &mut a.proposal)?;
    let ProposalAction::Split { numerator, denominator } = a.proposal.action else { return err!(VaultError::BadProposal) };
    apply_split(&a.basket, a.share_mint.as_ref(), a.token_program.as_ref(), numerator, denominator)
}

/// Runs a passed Stops. The payer covers the stop account's rent the first time. Permissionless.
/// Remaining accounts: each stop's `AssetPrice`, in order, fresh; a reference above its price is
/// lowered to it, so no stop starts out armed.
pub fn community_execute_stops<'info>(ctx: Context<'_, '_, 'info, 'info, CommunityExecuteStops<'info>>) -> Result<()> {
    let a = &mut *ctx.accounts;
    take_ready(&a.basket, &a.community, &mut a.proposal)?;
    let ProposalAction::Stops { stops } = &a.proposal.action else { return err!(VaultError::BadProposal) };
    let mut stops = stops.clone();
    check_stops(&a.basket, &stops)?;
    hold_to_price(&mut stops, ctx.remaining_accounts, true)?;
    write_stops(&mut a.stop_loss, a.basket.key(), ctx.bumps.stop_loss, &stops);
    Ok(())
}

/// Runs a passed GoSmart, Mandate, or Leave of a basket handed to the strategist. The vote has
/// served the notice a creator's mandate waits out, so each applies at once. The payer covers the
/// rules account's rent on GoSmart; leaving returns it to the creator. Permissionless.
pub fn community_execute_smart(ctx: Context<CommunityExecuteSmart>) -> Result<()> {
    let a = &mut *ctx.accounts;
    take_ready(&a.basket, &a.community, &mut a.proposal)?;
    let s = &mut a.smart;
    match a.proposal.action {
        ProposalAction::GoSmart { mandate_hash, guard } => {
            require!(a.basket.control == ControlMode::Community, VaultError::NotExecutable);
            a.basket.control = ControlMode::Smart;
            s.bump = ctx.bumps.smart;
            s.basket = a.basket.key();
            s.mandate_hash = mandate_hash;
            s.guard = guard;
            s.hour = Clock::get()?.unix_timestamp / 3_600;
            s.turnover = [0; TURNOVER_HOURS];
        }
        ProposalAction::Mandate { mandate_hash, guard } => {
            require!(a.basket.control == ControlMode::Smart, VaultError::NotExecutable);
            s.mandate_hash = mandate_hash;
            s.guard = guard;
        }
        ProposalAction::Leave => {
            require!(a.basket.control == ControlMode::Smart, VaultError::NotExecutable);
            leave(&mut a.basket, &mut a.community)?;
            return a.smart.close(a.creator.to_account_info());
        }
        _ => return err!(VaultError::BadProposal),
    }
    Ok(())
}

/// Drops an asset a community basket has sold out (0% target, no units). Permissionless: no holder's
/// claim changes. Not while a proposal is open (a Reweight pins the basket's assets, and removing
/// one would void it), nor for REMOVE_GRACE_SECS after a vote added an asset (which sits at 0%
/// with nothing in it until the Reweight that weights it runs). Close finished proposals first.
pub fn community_remove(ctx: Context<CommunityRemove>, index: u8) -> Result<()> {
    require!(ctx.accounts.basket.control == ControlMode::Community, VaultError::NotCommunity);
    require!(ctx.accounts.community.open == 0, VaultError::RemovalWaitsForVotes);
    let now = Clock::get()?.unix_timestamp;
    require!(now >= ctx.accounts.basket.vote_added_at() + REMOVE_GRACE_SECS, VaultError::AssetJustAdded);
    drop_emptied(&mut ctx.accounts.basket, index)
}

/// Closes a proposal that's done (run, failed, lapsed, or voided by the basket leaving Community),
/// its rent back to the proposer, and their `OpenProposal` with it, so they can propose again.
/// Permissionless. A proposal opened before `OpenProposal` existed has none to close.
pub fn community_close(ctx: Context<CommunityClose>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let p = &ctx.accounts.proposal;
    let done = p.executed || p.epoch != ctx.accounts.community.epoch || now > p.exec_until() || (now >= p.vote_end() && !p.passed());
    require!(done, VaultError::ProposalStillOpen);
    let (id, basket) = (p.id, p.basket);
    let c = &mut ctx.accounts.community;
    c.open = c.open.saturating_sub(1);

    let o = ctx.accounts.open_proposal.to_account_info();
    if o.owner == &crate::ID && o.data_len() > 0 {
        let marker = OpenProposal::try_deserialize(&mut &o.try_borrow_data()?[..])?;
        if marker.basket == basket && marker.id == id {
            let proposer = ctx.accounts.proposer.to_account_info();
            let lamports = o.lamports();
            **o.try_borrow_mut_lamports()? = 0;
            **proposer.try_borrow_mut_lamports()? = proposer.lamports().checked_add(lamports).ok_or(VaultError::MathOverflow)?;
            o.resize(0)?;
            o.assign(&anchor_lang::solana_program::system_program::ID);
        }
    }
    Ok(())
}

/// Closes a vote record once its vote has ended, its rent back to the voter. Permissionless.
pub fn community_close_vote(ctx: Context<CommunityCloseVote>) -> Result<()> {
    require!(Clock::get()?.unix_timestamp >= ctx.accounts.vote.vote_end, VaultError::ProposalStillOpen);
    Ok(())
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct SetCommunityOpen<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// Grown by hand to hold the flags after its fields (see PROTOCOL_FLAGS_OFFSET).
    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Box<Account<'info, Protocol>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ShareStakeIn<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: the wallet's share account; the token program checks it as it moves.
    #[account(mut)]
    pub wallet_shares: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the owner of staked shares.
    #[account(seeds = [SHARE_ESCROW_SEED], bump)]
    pub escrow_authority: UncheckedAccount<'info>,
    /// CHECK: the escrow's canonical account for this share mint, checked in the instruction.
    #[account(mut)]
    pub escrow: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = wallet, space = 8 + ShareStake::INIT_SPACE, seeds = [SHARE_STAKE_SEED, basket.key().as_ref(), wallet.key().as_ref()], bump)]
    pub share_stake: Box<Account<'info, ShareStake>>,
    pub share_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ShareStakeOut<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: the wallet's share account; the token program checks it as it moves.
    #[account(mut)]
    pub wallet_shares: UncheckedAccount<'info>,
    /// CHECK: address-only PDA; signs as the owner of staked shares.
    #[account(seeds = [SHARE_ESCROW_SEED], bump)]
    pub escrow_authority: UncheckedAccount<'info>,
    /// CHECK: the escrow's canonical account for this share mint, checked in the instruction.
    #[account(mut)]
    pub escrow: UncheckedAccount<'info>,
    #[account(mut, seeds = [SHARE_STAKE_SEED, basket.key().as_ref(), wallet.key().as_ref()], bump = share_stake.bump)]
    pub share_stake: Box<Account<'info, ShareStake>>,
    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct EnterCommunity<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(mut, has_one = creator @ VaultError::NotMandateAuthority)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(init_if_needed, payer = creator, space = 8 + Community::INIT_SPACE, seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump)]
    pub community: Box<Account<'info, Community>>,
    /// Read for its flags.
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(action: ProposalAction)]
pub struct CommunityPropose<'info> {
    #[account(mut)]
    pub proposer: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    #[account(seeds = [SHARE_STAKE_SEED, basket.key().as_ref(), proposer.key().as_ref()], bump = share_stake.bump)]
    pub share_stake: Box<Account<'info, ShareStake>>,
    #[account(mut, seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    /// The proposer's one open proposal on this basket: refused if it's still there.
    #[account(init_if_needed, payer = proposer, space = 8 + OpenProposal::INIT_SPACE, seeds = [OPEN_SEED, basket.key().as_ref(), proposer.key().as_ref()], bump)]
    pub open_proposal: Box<Account<'info, OpenProposal>>,
    #[account(
        init,
        payer = proposer,
        space = Proposal::BASE_SPACE + action.try_to_vec()?.len(),
        seeds = [PROPOSAL_SEED, basket.key().as_ref(), &community.next_id.to_le_bytes()],
        bump
    )]
    pub proposal: Box<Account<'info, Proposal>>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    #[account(mut, seeds = [CREATION_BURN_SEED], bump = creation_burn.bump)]
    pub creation_burn: Box<Account<'info, CreationBurn>>,
    /// CHECK: GRAVI, matched against the protocol's mint in `burn_gravi`. Needed only when this
    /// proposal burns.
    #[account(mut)]
    pub gravi_mint: Option<UncheckedAccount<'info>>,
    /// CHECK: the proposer's GRAVI account; the token program checks it as it burns.
    #[account(mut)]
    pub proposer_gravi: Option<UncheckedAccount<'info>>,
    pub gravi_token_program: Option<Interface<'info, TokenInterface>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CommunityVote<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut)]
    pub proposal: Box<Account<'info, Proposal>>,
    #[account(mut, seeds = [SHARE_STAKE_SEED, proposal.basket.as_ref(), wallet.key().as_ref()], bump = share_stake.bump)]
    pub share_stake: Box<Account<'info, ShareStake>>,
    #[account(init, payer = wallet, space = 8 + VoteRecord::INIT_SPACE, seeds = [VOTE_SEED, proposal.key().as_ref(), wallet.key().as_ref()], bump)]
    pub vote: Box<Account<'info, VoteRecord>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CommunityExecute<'info> {
    #[account(mut)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(mut, seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    #[account(mut, has_one = basket)]
    pub proposal: Box<Account<'info, Proposal>>,
}

#[derive(Accounts)]
pub struct CommunityExecuteAdd<'info> {
    #[account(mut)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    #[account(mut, has_one = basket)]
    pub proposal: Box<Account<'info, Proposal>>,
    /// CHECK: the asset's mint, matched against the proposal and its listing entry.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: the asset's listing entry, read by `load_asset_entry`.
    pub asset_entry: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for the asset, checked by `read_vault` (canonical ATA).
    pub vault: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CommunityExecuteSplit<'info> {
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    #[account(mut, has_one = basket)]
    pub proposal: Box<Account<'info, Proposal>>,
    /// CHECK: the basket's share mint (has_one above); Token-2022 validates the extension.
    #[account(mut)]
    pub share_mint: UncheckedAccount<'info>,
    /// CHECK: the Token-2022 program.
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CommunityExecuteStops<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub basket: Box<Account<'info, Basket>>,
    #[account(seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    #[account(mut, has_one = basket)]
    pub proposal: Box<Account<'info, Proposal>>,
    #[account(init_if_needed, payer = payer, space = 8 + StopLoss::INIT_SPACE, seeds = [STOP_SEED, basket.key().as_ref()], bump)]
    pub stop_loss: Box<Account<'info, StopLoss>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CommunityExecuteSmart<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(mut, seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    #[account(mut, has_one = basket)]
    pub proposal: Box<Account<'info, Proposal>>,
    #[account(init_if_needed, payer = payer, space = 8 + Smart::INIT_SPACE, seeds = [SMART_SEED, basket.key().as_ref()], bump)]
    pub smart: Box<Account<'info, Smart>>,
    /// CHECK: the basket's creator, who gets the rules account's rent back on leaving.
    #[account(mut, address = basket.creator @ VaultError::NotMandateAuthority)]
    pub creator: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CommunityRemove<'info> {
    #[account(mut)]
    pub basket: Box<Account<'info, Basket>>,
    #[account(seeds = [COMMUNITY_SEED, basket.key().as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
}

#[derive(Accounts)]
pub struct CommunityClose<'info> {
    #[account(mut, seeds = [COMMUNITY_SEED, proposal.basket.as_ref()], bump = community.bump)]
    pub community: Box<Account<'info, Community>>,
    #[account(mut, close = proposer)]
    pub proposal: Box<Account<'info, Proposal>>,
    /// CHECK: the proposer, who gets the rent back.
    #[account(mut, address = proposal.proposer)]
    pub proposer: UncheckedAccount<'info>,
    /// CHECK: the proposer's `OpenProposal` on this basket, closed by hand if it's this proposal's
    /// (a proposal opened before they existed has none).
    #[account(mut, seeds = [OPEN_SEED, proposal.basket.as_ref(), proposal.proposer.as_ref()], bump)]
    pub open_proposal: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CommunityCloseVote<'info> {
    #[account(mut, close = wallet)]
    pub vote: Box<Account<'info, VoteRecord>>,
    /// CHECK: the voter, who gets the rent back.
    #[account(mut, address = vote.wallet)]
    pub wallet: UncheckedAccount<'info>,
}
