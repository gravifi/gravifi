//! The creator's cut: a share of every mint of their basket, at most `MAX_CREATOR_CUT_BPS`, in
//! place of the yearly fee. Redeems never pay it, so a holder already in can't be charged more
//! by a later rise. A cut going up waits `CUT_NOTICE_SECS` once the basket has shares, as a buyer's
//! slippage floor is checked before fees; going down applies at once, and calls off any rise
//! still waiting (the basket remembers it, so the rise is void whether or not its notice account
//! was passed).
//!
//! What's owed sits in the basket's treasury shares and is counted on the basket
//! (`Basket::creator_unclaimed`), so a mint names no new account. `claim_creator_cut` moves it to
//! the creator, and `treasury_withdraw` leaves it alone.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::instructions::basket::authorise_mandate;
use crate::params::{self, Params, PARAMS_ID};
use crate::state::*;
use crate::token_util::is_ata;

/// A cut replaces the yearly fee. The app runs `accrue_fees` earlier in the same transaction to
/// collect what's streamed so far, as it does for `set_policy`.
fn stop_yearly_fee(basket: &mut Basket, now: i64) {
    if basket.mgmt_fee_bps != 0 {
        basket.mgmt_fee_bps = 0;
        basket.last_fee_accrual = now;
    }
}

/// Sets the cut at once: down at any time, or to anything before the basket's first deposit (an
/// immutable basket only then). It calls off a pending rise: passing the rise's notice closes it,
/// the rent back to the creator; left out, the rise is void and `apply_cut_rise` refuses it.
/// Creator only.
pub fn set_creator_cut(ctx: Context<SetCreatorCut>, bps: u16) -> Result<()> {
    require!(bps <= MAX_CREATOR_CUT_BPS, VaultError::FeeTooHigh);
    let basket = &mut ctx.accounts.basket;
    if basket.control == ControlMode::Immutable {
        require_keys_eq!(ctx.accounts.authority.key(), basket.manager(), VaultError::NotMandateAuthority);
        require!(!basket.seeded, VaultError::BasketImmutable);
    } else {
        authorise_mandate(basket, &ctx.accounts.authority)?;
    }
    require!(!basket.seeded || bps <= basket.creator_cut_bps, VaultError::CutRiseNeedsNotice);
    basket.creator_cut_bps = bps;
    basket.set_cut_rise_void(true);
    if bps > 0 {
        stop_yearly_fee(basket, Clock::get()?.unix_timestamp);
    }
    Ok(())
}

/// Asks for a higher cut on a basket with shares out, from the `CUT_NOTICE_SECS` param (a day)
/// from now, the time written on the notice. One at a
/// time: `set_creator_cut` with the notice cancels it first. The yearly fee stops now. Creator
/// only, who pays the notice's rent and gets it back when it's applied or cancelled.
pub fn propose_cut_rise(ctx: Context<ProposeCutRise>, bps: u16) -> Result<()> {
    require!(bps <= MAX_CREATOR_CUT_BPS, VaultError::FeeTooHigh);
    let basket = &mut ctx.accounts.basket;
    authorise_mandate(basket, &ctx.accounts.authority)?;
    require!(basket.seeded, VaultError::NotSeeded);
    require!(bps > basket.creator_cut_bps, VaultError::BadParameter);
    let now = Clock::get()?.unix_timestamp;
    stop_yearly_fee(basket, now);
    basket.set_cut_rise_void(false);
    let notice = &mut ctx.accounts.notice;
    notice.bump = ctx.bumps.notice;
    notice.basket = basket.key();
    notice.bps = bps;
    notice.at = now + Params::load(&ctx.accounts.params)?.secs(params::CUT_NOTICE_SECS);
    Ok(())
}

/// Applies a rise whose notice has run out, the notice's rent back to the creator. Permissionless:
/// the creator already asked for it, and everyone has had the notice. Refused once the cut was set
/// or lowered after it was asked for (by the creator or a vote), and when it's no higher than the
/// cut in force: a rise must never land on top of a later lowering.
pub fn apply_cut_rise(ctx: Context<ApplyCutRise>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(now >= ctx.accounts.notice.at, VaultError::CutNoticeRunning);
    // Once holders run it, a vote can only lower the cut: a rise asked for before doesn't apply.
    require!(!ctx.accounts.basket.by_vote(), VaultError::CommunityManaged);
    let bps = ctx.accounts.notice.bps;
    let basket = &mut ctx.accounts.basket;
    require!(!basket.cut_rise_void() && bps > basket.creator_cut_bps, VaultError::CutRiseVoid);
    basket.creator_cut_bps = bps;
    stop_yearly_fee(basket, now);
    Ok(())
}

/// Moves what the creator is owed from the treasury's shares to theirs. Permissionless, like
/// `accrue_fees`: the destination is pinned to the creator. Pays what the treasury's account holds
/// of it, the rest staying owed (shares a side account took before the treasury's was pinned
/// aren't in it).
pub fn claim_creator_cut(ctx: Context<ClaimCreatorCut>) -> Result<()> {
    let owed = ctx.accounts.basket.owed_to_creator();
    // A basket that's been vote-run: only the creator's part of it (`stake_cut.rs`); the stakers'
    // stays. Its extension account must be passed.
    let rec = if ctx.accounts.basket.flag(STAKER_CUT) {
        let ext_ai = ctx.accounts.ext.as_ref().ok_or(VaultError::BadExt)?.to_account_info();
        Some((crate::instructions::stake_cut::load(&ctx.accounts.basket, &ext_ai)?.unwrap_or_default(), ext_ai))
    } else {
        None
    };
    let mine = rec.as_ref().map_or(owed, |(r, _)| r.creator_part(&ctx.accounts.basket).min(owed));
    let amount = mine.min(ctx.accounts.treasury_shares.amount);
    if amount == 0 {
        return Ok(());
    }
    if let Some((mut r, ext_ai)) = rec {
        if ctx.accounts.basket.by_vote() {
            r.base -= amount;
            crate::instructions::stake_cut::store(&ctx.accounts.basket.key(), &ext_ai, &r)?;
        }
    }
    ctx.accounts.basket.creator_unclaimed = owed - amount;
    let bump = [ctx.accounts.protocol.bump];
    let seeds: [&[u8]; 2] = [PROTOCOL_SEED, &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.share_token_program.to_account_info(),
            token_interface::TransferChecked {
                from: ctx.accounts.treasury_shares.to_account_info(),
                mint: ctx.accounts.share_mint.to_account_info(),
                to: ctx.accounts.creator_shares.to_account_info(),
                authority: ctx.accounts.protocol.to_account_info(),
            },
            &[&seeds],
        ),
        amount,
        ctx.accounts.share_mint.decimals,
    )
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct SetCreatorCut<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    /// A pending rise to cancel, if any.
    #[account(mut, close = authority, has_one = basket, seeds = [CUT_NOTICE_SEED, basket.key().as_ref()], bump = notice.bump)]
    pub notice: Option<Account<'info, CutNotice>>,
}

#[derive(Accounts)]
pub struct ProposeCutRise<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    #[account(init, payer = authority, space = 8 + CutNotice::INIT_SPACE, seeds = [CUT_NOTICE_SEED, basket.key().as_ref()], bump)]
    pub notice: Account<'info, CutNotice>,
    pub system_program: Program<'info, System>,
    /// CHECK: the params account (`params.rs`); it may not exist, and then every value is its default.
    #[account(address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ApplyCutRise<'info> {
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    #[account(mut, close = creator, has_one = basket, seeds = [CUT_NOTICE_SEED, basket.key().as_ref()], bump = notice.bump)]
    pub notice: Account<'info, CutNotice>,
    /// CHECK: the basket's manager (`Basket::manager`), who gets the notice's rent back.
    #[account(mut, address = basket.manager() @ VaultError::NotMandateAuthority)]
    pub creator: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ClaimCreatorCut<'info> {
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,

    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,

    pub share_mint: InterfaceAccount<'info, MintI>,

    #[account(mut, token::mint = share_mint, token::authority = protocol, constraint = is_ata(&treasury_shares.to_account_info(), &protocol.key(), &share_mint.key()) @ VaultError::NotBasketVault)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    /// Pinned to the manager (`Basket::manager`: the creator, unless they handed the basket over):
    /// anyone may call this, and it must not pay anyone else.
    #[account(mut, token::mint = share_mint, token::authority = basket.manager())]
    pub creator_shares: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
    /// CHECK: `[EXT_SEED, basket]`, needed once the basket has a stakers' cut record (`stake_cut.rs`).
    #[account(mut)]
    pub ext: Option<UncheckedAccount<'info>>,
}
