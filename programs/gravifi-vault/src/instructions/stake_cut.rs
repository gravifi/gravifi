//! A vote-run basket's creator cut goes to its share stakers (the team, 8 Nov 2026: "I want the 0.6%
//! fees to go to Shares stakers instead of the creator"). A creator-run basket keeps paying its
//! creator.
//!
//! Nothing changes in a mint: the cut still lands in the basket's treasury shares and is counted in
//! `creator_unclaimed`. A record in the basket's extension account splits that count:
//! - `base`: the creator's part, frozen when the basket is handed to its holders (`enter_community`);
//! - `allocated`: what has been shared over the staked shares and not yet claimed;
//! - `frozen`: what accrued while it was vote-run and wasn't shared yet when it left (`leave`);
//! - `acc`: shares owed per staked share, x `SCALE`, only ever rising.
//! While vote-run, everything in `creator_unclaimed` past `base` and `allocated` is new for the
//! stakers; `sync` shares it over the shares staked then (the escrow's balance). Every change of a
//! stake syncs first, so what's shared always goes to the stakes that were there while it accrued.
//!
//! Each stake keeps its position after its `ShareStake` fields, which the account grows to hold
//! the first time: [debt u128][owed u64]. A stake without them has never changed since the record
//! began, so its debt is 0 then, which is right: `acc` starts at 0.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenInterface};

use crate::errors::VaultError;
use crate::ext;
use crate::state::*;
use crate::token_util::*;

/// The record's tag in the extension account.
pub const CUT_TAG: u16 = 1;
const REC_LEN: usize = 40;
pub const SCALE: u128 = 1_000_000_000_000;
/// Where a stake's position starts in its account, and its length.
pub const STAKE_TAIL_AT: usize = 8 + ShareStake::INIT_SPACE;
const STAKE_TAIL_LEN: usize = 24;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CutRec {
    pub base: u64,
    pub allocated: u64,
    pub frozen: u64,
    pub acc: u128,
}

impl CutRec {
    fn read(b: &[u8]) -> Self {
        let w = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        Self { base: w(0), allocated: w(8), frozen: w(16), acc: u128::from_le_bytes(b[24..40].try_into().unwrap()) }
    }

    fn bytes(&self) -> [u8; REC_LEN] {
        let mut b = [0u8; REC_LEN];
        b[0..8].copy_from_slice(&self.base.to_le_bytes());
        b[8..16].copy_from_slice(&self.allocated.to_le_bytes());
        b[16..24].copy_from_slice(&self.frozen.to_le_bytes());
        b[24..40].copy_from_slice(&self.acc.to_le_bytes());
        b
    }

    /// What the creator may claim of `creator_unclaimed`: their frozen part while vote-run, else all
    /// but the stakers'.
    pub fn creator_part(&self, basket: &Basket) -> u64 {
        if basket.by_vote() {
            self.base
        } else {
            basket.creator_unclaimed.saturating_sub(self.allocated).saturating_sub(self.frozen)
        }
    }
}

/// The record, if the basket has one (`STAKER_CUT`), read from `ext`.
pub fn load(basket: &Account<Basket>, ext_ai: &AccountInfo) -> Result<Option<CutRec>> {
    if !basket.flag(STAKER_CUT) {
        return Ok(None);
    }
    let (expected, _) = ext::address(&basket.key());
    require_keys_eq!(*ext_ai.key, expected, VaultError::BadExt);
    let d = ext_ai.try_borrow_data()?;
    ext::check(&d, &basket.key())?;
    Ok(Some(ext::get(&d, CUT_TAG).map(CutRec::read).unwrap_or_default()))
}

pub fn store(basket: &Pubkey, ext_ai: &AccountInfo, rec: &CutRec) -> Result<()> {
    let mut d = ext_ai.try_borrow_mut_data()?;
    ext::check(&d, basket)?;
    ext::put(&mut d, CUT_TAG, &rec.bytes())
}

/// Starts the record for a basket going to (or found as) vote-run, creating the extension account
/// if needed (`payer` pays): the creator's part is what's owed them now.
pub fn begin<'info>(basket: &mut Account<'info, Basket>, ext_ai: &AccountInfo<'info>, payer: &AccountInfo<'info>, system: &AccountInfo<'info>) -> Result<CutRec> {
    let key = basket.key();
    ext::ensure(ext_ai, &key, payer, system)?;
    let rec = if basket.flag(STAKER_CUT) {
        let mut r = load(basket, ext_ai)?.unwrap_or_default();
        // Back to vote-run: what's still owed the stakers stays theirs (`frozen` is then counted
        // with what accrues next, as it's past `base` and `allocated`); the rest is the creator's.
        r.base = basket.creator_unclaimed.saturating_sub(r.allocated).saturating_sub(r.frozen);
        r.frozen = 0;
        r
    } else {
        CutRec { base: basket.creator_unclaimed, ..Default::default() }
    };
    basket.set_flag(STAKER_CUT, true);
    store(&key, ext_ai, &rec)?;
    Ok(rec)
}

/// Shares what accrued for the stakers since the last sync over `staked` shares. What accrued
/// with nobody staked is the treasury's (it leaves `creator_unclaimed`, so the treasury's shares
/// count it as theirs): the first to stake never takes the backlog.
pub fn sync(basket: &mut Basket, rec: &mut CutRec, staked: u64) -> Result<()> {
    let new = if basket.by_vote() {
        basket.creator_unclaimed.saturating_sub(rec.base).saturating_sub(rec.allocated)
    } else {
        rec.frozen
    };
    if new == 0 {
        return Ok(());
    }
    if staked == 0 {
        basket.creator_unclaimed -= new;
        if !basket.by_vote() {
            rec.frozen = 0;
        }
        return Ok(());
    }
    let per = (new as u128).checked_mul(SCALE).ok_or(VaultError::MathOverflow)? / staked as u128;
    // Only what the stakes can be paid at `per` (rounded down): the rest waits for the next sync,
    // instead of staying counted as theirs and never claimable.
    let shared = u64::try_from(per * staked as u128 / SCALE).map_err(|_| VaultError::MathOverflow)?;
    rec.acc = rec.acc.checked_add(per).ok_or(VaultError::MathOverflow)?;
    rec.allocated = rec.allocated.checked_add(shared).ok_or(VaultError::MathOverflow)?;
    if !basket.by_vote() {
        rec.frozen = new - shared;
    }
    Ok(())
}

/// What a basket leaving vote-run owed its stakers and hadn't shared yet: kept for them.
pub fn freeze(basket: &Basket, rec: &mut CutRec) {
    rec.frozen = rec.frozen.saturating_add(basket.creator_unclaimed.saturating_sub(rec.base).saturating_sub(rec.allocated));
}

/// A stake's position: (debt, owed).
pub fn stake_tail(ai: &AccountInfo) -> Result<(u128, u64)> {
    let d = ai.try_borrow_data()?;
    Ok(match d.get(STAKE_TAIL_AT..STAKE_TAIL_AT + STAKE_TAIL_LEN) {
        Some(t) => (u128::from_le_bytes(t[..16].try_into().unwrap()), u64::from_le_bytes(t[16..24].try_into().unwrap())),
        None => (0, 0),
    })
}

/// Writes a stake's position, growing its account the first time (`payer` pays the rent).
pub fn put_stake_tail<'info>(ai: &AccountInfo<'info>, payer: &AccountInfo<'info>, system: &AccountInfo<'info>, debt: u128, owed: u64) -> Result<()> {
    grow(ai, payer, system, STAKE_TAIL_AT + STAKE_TAIL_LEN)?;
    let mut d = ai.try_borrow_mut_data()?;
    d[STAKE_TAIL_AT..STAKE_TAIL_AT + 16].copy_from_slice(&debt.to_le_bytes());
    d[STAKE_TAIL_AT + 16..STAKE_TAIL_AT + 24].copy_from_slice(&owed.to_le_bytes());
    Ok(())
}

/// What `amount` staked has earned at `acc`, on top of what it was owed: the new owed.
pub fn earned(amount: u64, acc: u128, debt: u128, owed: u64) -> Result<u64> {
    let gross = (amount as u128).checked_mul(acc).ok_or(VaultError::MathOverflow)? / SCALE;
    let add = u64::try_from(gross.saturating_sub(debt / SCALE)).map_err(|_| VaultError::MathOverflow)?;
    owed.checked_add(add).ok_or(VaultError::MathOverflow.into())
}

/// The debt that marks `amount` staked as paid up to `acc` (x SCALE, so rounding never pays twice).
pub fn debt_of(amount: u64, acc: u128) -> Result<u128> {
    let gross = (amount as u128).checked_mul(acc).ok_or(VaultError::MathOverflow)? / SCALE;
    gross.checked_mul(SCALE).ok_or(VaultError::MathOverflow.into())
}

/// Brings a stake up to date before its amount changes: syncs the basket's record (if it has one)
/// and moves what the stake earned into its `owed`. Returns the record (to store) and the stake's
/// new position for the amount it will have.
pub fn before_change<'info>(
    basket: &mut Account<'info, Basket>,
    ext_ai: &AccountInfo<'info>,
    stake_ai: &AccountInfo<'info>,
    staked_total: u64,
    amount_now: u64,
) -> Result<Option<(CutRec, u64)>> {
    let Some(mut rec) = load(basket, ext_ai)? else { return Ok(None) };
    sync(basket, &mut rec, staked_total)?;
    let (debt, owed) = stake_tail(stake_ai)?;
    let owed = earned(amount_now, rec.acc, debt, owed)?;
    Ok(Some((rec, owed)))
}

// ---------------------------------------------------------------------------- instructions

/// Starts or brings up to date a vote-run basket's stakers' record, and shares what accrued over the
/// shares staked now. Permissionless (`payer` pays the extension account's rent the first time):
/// the release runs it once for each basket already vote-run.
pub fn sync_stake_cut(ctx: Context<SyncStakeCut>) -> Result<()> {
    let staked = read_vault(&ctx.accounts.escrow.to_account_info(), &ctx.accounts.escrow_authority.key(), &ctx.accounts.share_mint.key())?.amount;
    let a = &mut *ctx.accounts;
    let ext_ai = a.ext.to_account_info();
    let mut rec = if a.basket.flag(STAKER_CUT) {
        load(&a.basket, &ext_ai)?.unwrap_or_default()
    } else {
        require!(a.basket.by_vote(), VaultError::NotExecutable);
        begin(&mut a.basket, &ext_ai, &a.payer.to_account_info(), &a.system_program.to_account_info())?
    };
    sync(&mut a.basket, &mut rec, staked)?;
    store(&a.basket.key(), &ext_ai, &rec)
}

/// Pays a stake what the basket's cut earned it, in the basket's shares, from the treasury's.
pub fn claim_stake_cut(ctx: Context<ClaimStakeCut>) -> Result<()> {
    let staked = read_vault(&ctx.accounts.escrow.to_account_info(), &ctx.accounts.escrow_authority.key(), &ctx.accounts.share_mint.key())?.amount;
    let a = &mut *ctx.accounts;
    let ext_ai = a.ext.to_account_info();
    let stake_ai = a.share_stake.to_account_info();
    let amount = a.share_stake.amount;
    let Some((mut rec, owed)) = before_change(&mut a.basket, &ext_ai, &stake_ai, staked, amount)? else { return err!(VaultError::NotExecutable) };
    let pay = owed.min(a.treasury_shares.amount).min(rec.allocated);
    rec.allocated -= pay;
    a.basket.creator_unclaimed = a.basket.creator_unclaimed.saturating_sub(pay);
    store(&a.basket.key(), &ext_ai, &rec)?;
    put_stake_tail(&stake_ai, &a.wallet.to_account_info(), &a.system_program.to_account_info(), debt_of(amount, rec.acc)?, owed - pay)?;
    if pay > 0 {
        let bump = [a.protocol.bump];
        let seeds: [&[u8]; 2] = [PROTOCOL_SEED, &bump];
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                a.share_token_program.to_account_info(),
                token_interface::TransferChecked {
                    from: a.treasury_shares.to_account_info(),
                    mint: a.share_mint.to_account_info(),
                    to: a.wallet_shares.to_account_info(),
                    authority: a.protocol.to_account_info(),
                },
                &[&seeds],
            ),
            pay,
            a.share_mint.decimals,
        )?;
    }
    // A stake taken back in full, paid in full (its USDC too): its account closes, the rent back.
    if amount == 0 && owed == pay && usdc_tail(&stake_ai)?.1 == 0 {
        return a.share_stake.close(a.wallet.to_account_info());
    }
    Ok(())
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct SyncStakeCut<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: `[EXT_SEED, basket]`, created or checked by `ext`.
    #[account(mut)]
    pub ext: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the owner of staked shares.
    #[account(seeds = [SHARE_ESCROW_SEED], bump)]
    pub escrow_authority: UncheckedAccount<'info>,
    /// CHECK: the escrow's canonical account for this share mint, checked by `read_vault`.
    pub escrow: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimStakeCut<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(mut, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: `[EXT_SEED, basket]`, checked by `ext`.
    #[account(mut)]
    pub ext: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the owner of staked shares.
    #[account(seeds = [SHARE_ESCROW_SEED], bump)]
    pub escrow_authority: UncheckedAccount<'info>,
    /// CHECK: the escrow's canonical account for this share mint, checked by `read_vault`.
    pub escrow: UncheckedAccount<'info>,
    #[account(mut, seeds = [SHARE_STAKE_SEED, basket.key().as_ref(), wallet.key().as_ref()], bump = share_stake.bump)]
    pub share_stake: Box<Account<'info, ShareStake>>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    #[account(mut, token::mint = share_mint, token::authority = protocol, constraint = is_ata(&treasury_shares.to_account_info(), &protocol.key(), &share_mint.key()) @ VaultError::NotBasketVault)]
    pub treasury_shares: Box<InterfaceAccount<'info, anchor_spl::token_interface::TokenAccount>>,
    /// CHECK: the wallet's share account; the token program checks it as it moves.
    #[account(mut)]
    pub wallet_shares: UncheckedAccount<'info>,
    pub share_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

// ------------------------------------------------------------------------- the cut in USDC

// Since the fees went to USDC (the team, 9 Nov 2026: "community basket fees go to holders staking the
// shares of the basket, those have to be paid in native as well"), a vote-run basket's cut on a mint
// paid in USDC lands in the basket's USDC pot: the USDC account of `[CUT_POT_SEED, basket]`. It's
// shared over the staked shares the same way, in a second record (`USDC_TAG`): what the pot holds
// past `allocated` is new, `acc` is USDC per staked share x `SCALE`. Each stake keeps a second
// position, [debt u128][owed u64], right after the first. What was owed in shares before stays owed
// in shares (the record above).

pub const USDC_TAG: u16 = 2;
const USDC_REC_LEN: usize = 32;
pub const USDC_TAIL_AT: usize = STAKE_TAIL_AT + STAKE_TAIL_LEN;
/// A stake account with room for both positions.
pub const STAKE_FULL_LEN: usize = USDC_TAIL_AT + STAKE_TAIL_LEN;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct UsdcRec {
    pub allocated: u64,
    pub acc: u128,
    /// What came in with nobody staked: the treasury's, moved there by `pot_to_treasury`.
    pub treasury: u64,
}

impl UsdcRec {
    fn read(b: &[u8]) -> Self {
        let treasury = b.get(24..32).map_or(0, |t| u64::from_le_bytes(t.try_into().unwrap()));
        Self { allocated: u64::from_le_bytes(b[0..8].try_into().unwrap()), acc: u128::from_le_bytes(b[8..24].try_into().unwrap()), treasury }
    }

    fn bytes(&self) -> [u8; USDC_REC_LEN] {
        let mut b = [0u8; USDC_REC_LEN];
        b[0..8].copy_from_slice(&self.allocated.to_le_bytes());
        b[8..24].copy_from_slice(&self.acc.to_le_bytes());
        b[24..32].copy_from_slice(&self.treasury.to_le_bytes());
        b
    }
}

/// The pot's owner (a PDA, signing only in `claim_stake_cut_usdc`) and the pot (its USDC ATA).
pub fn pot_of(basket: &Pubkey) -> (Pubkey, u8, Pubkey) {
    let (owner, bump) = Pubkey::find_program_address(&[crate::instructions::mint_redeem::CUT_POT_SEED, basket.as_ref()], &crate::ID);
    let usdc = crate::instructions::mint_redeem::USDC_MINT;
    let (pot, _) = Pubkey::find_program_address(&[owner.as_ref(), anchor_spl::token::ID.as_ref(), usdc.as_ref()], &ATA_PROGRAM_ID);
    (owner, bump, pot)
}

/// What the pot at `ai` holds (0 if it isn't created yet). Refuses any other address.
pub fn pot_balance(ai: &AccountInfo, basket: &Pubkey) -> Result<u64> {
    let (owner, _, pot) = pot_of(basket);
    require_keys_eq!(*ai.key, pot, VaultError::BadPot);
    if ai.data_len() == 0 {
        return Ok(0);
    }
    Ok(movable_balance(ai, &crate::instructions::mint_redeem::USDC_MINT, &owner).ok_or(VaultError::BadPot)?)
}

pub fn load_usdc(basket: &Pubkey, ext_ai: &AccountInfo) -> Result<UsdcRec> {
    let d = ext_ai.try_borrow_data()?;
    ext::check(&d, basket)?;
    Ok(ext::get(&d, USDC_TAG).map(UsdcRec::read).unwrap_or_default())
}

pub fn store_usdc(basket: &Pubkey, ext_ai: &AccountInfo, rec: &UsdcRec) -> Result<()> {
    let mut d = ext_ai.try_borrow_mut_data()?;
    ext::check(&d, basket)?;
    ext::put(&mut d, USDC_TAG, &rec.bytes())
}

/// Shares what came into the pot since the last sync over `staked` shares; with nobody staked it's
/// the treasury's (`pot_to_treasury`), never the next staker's.
pub fn sync_usdc(rec: &mut UsdcRec, pot: u64, staked: u64) -> Result<()> {
    let new = pot.saturating_sub(rec.allocated).saturating_sub(rec.treasury);
    if new == 0 {
        return Ok(());
    }
    if staked == 0 {
        rec.treasury = rec.treasury.checked_add(new).ok_or(VaultError::MathOverflow)?;
        return Ok(());
    }
    let per = (new as u128).checked_mul(SCALE).ok_or(VaultError::MathOverflow)? / staked as u128;
    // As in `sync`: only what the stakes can be paid; the rest waits.
    let shared = u64::try_from(per * staked as u128 / SCALE).map_err(|_| VaultError::MathOverflow)?;
    rec.acc = rec.acc.checked_add(per).ok_or(VaultError::MathOverflow)?;
    rec.allocated = rec.allocated.checked_add(shared).ok_or(VaultError::MathOverflow)?;
    Ok(())
}

/// A stake's USDC position: (debt, owed).
pub fn usdc_tail(ai: &AccountInfo) -> Result<(u128, u64)> {
    let d = ai.try_borrow_data()?;
    Ok(match d.get(USDC_TAIL_AT..USDC_TAIL_AT + STAKE_TAIL_LEN) {
        Some(t) => (u128::from_le_bytes(t[..16].try_into().unwrap()), u64::from_le_bytes(t[16..24].try_into().unwrap())),
        None => (0, 0),
    })
}

/// Writes a stake's USDC position, growing its account the first time (`payer` pays the rent).
pub fn put_usdc_tail<'info>(ai: &AccountInfo<'info>, payer: &AccountInfo<'info>, system: &AccountInfo<'info>, debt: u128, owed: u64) -> Result<()> {
    grow(ai, payer, system, STAKE_FULL_LEN)?;
    let mut d = ai.try_borrow_mut_data()?;
    d[USDC_TAIL_AT..USDC_TAIL_AT + 16].copy_from_slice(&debt.to_le_bytes());
    d[USDC_TAIL_AT + 16..USDC_TAIL_AT + 24].copy_from_slice(&owed.to_le_bytes());
    Ok(())
}

/// Brings a stake's USDC position up to date before its amount changes, for a basket with a cut
/// record (`STAKER_CUT`): the pot must be the first remaining account then. Returns the record (to
/// store) and what the stake is owed.
pub fn before_change_usdc<'info>(
    basket: &mut Account<'info, Basket>,
    ext_ai: &AccountInfo<'info>,
    remaining: &[AccountInfo],
    stake_ai: &AccountInfo<'info>,
    staked_total: u64,
    amount_now: u64,
) -> Result<Option<(UsdcRec, u64)>> {
    if !basket.flag(STAKER_CUT) {
        return Ok(None);
    }
    let pot_ai = remaining.first().ok_or(VaultError::BadPot)?;
    let pot = pot_balance(pot_ai, &basket.key())?;
    let mut rec = load_usdc(&basket.key(), ext_ai)?;
    sync_usdc(&mut rec, pot, staked_total)?;
    let (debt, owed) = usdc_tail(stake_ai)?;
    let owed = earned(amount_now, rec.acc, debt, owed)?;
    Ok(Some((rec, owed)))
}

/// Pays a stake what the basket's cut earned it in USDC, from the basket's pot. Remaining accounts:
/// none. Anyone staked (or still owed) may claim their own.
pub fn claim_stake_cut_usdc(ctx: Context<ClaimStakeCutUsdc>) -> Result<()> {
    let staked = read_vault(&ctx.accounts.escrow.to_account_info(), &ctx.accounts.escrow_authority.key(), &ctx.accounts.share_mint.key())?.amount;
    let a = &mut *ctx.accounts;
    require!(a.basket.flag(STAKER_CUT), VaultError::NotExecutable);
    let basket_key = a.basket.key();
    let (ext_ai, stake_ai, pot_ai) = (a.ext.to_account_info(), a.share_stake.to_account_info(), a.pot.to_account_info());
    let pot = pot_balance(&pot_ai, &basket_key)?;
    let mut rec = load_usdc(&basket_key, &ext_ai)?;
    sync_usdc(&mut rec, pot, staked)?;
    let amount = a.share_stake.amount;
    let (debt, owed) = usdc_tail(&stake_ai)?;
    let owed = earned(amount, rec.acc, debt, owed)?;
    let pay = owed.min(pot).min(rec.allocated);
    rec.allocated -= pay;
    store_usdc(&basket_key, &ext_ai, &rec)?;
    put_usdc_tail(&stake_ai, &a.wallet.to_account_info(), &a.system_program.to_account_info(), debt_of(amount, rec.acc)?, owed - pay)?;
    if pay > 0 {
        let (_, bump, _) = pot_of(&basket_key);
        let b = [bump];
        let seeds: [&[u8]; 3] = [crate::instructions::mint_redeem::CUT_POT_SEED, basket_key.as_ref(), &b];
        anchor_spl::token::transfer(
            CpiContext::new_with_signer(
                a.token_program.to_account_info(),
                anchor_spl::token::Transfer { from: pot_ai, to: a.wallet_usdc.to_account_info(), authority: a.pot_authority.to_account_info() },
                &[&seeds],
            ),
            pay,
        )?;
    }
    // A stake taken back in full, paid in full on both sides: its account closes, the rent back.
    if amount == 0 && owed == pay && stake_tail(&stake_ai)?.1 == 0 {
        return a.share_stake.close(a.wallet.to_account_info());
    }
    Ok(())
}

/// Moves to the treasury's USDC what came into a basket's pot while nobody was staked. Permissionless:
/// it can only go there. Syncs first, so what came in with nobody staked since is counted too.
pub fn pot_to_treasury(ctx: Context<PotToTreasury>) -> Result<()> {
    let staked = read_vault(&ctx.accounts.escrow.to_account_info(), &ctx.accounts.escrow_authority.key(), &ctx.accounts.share_mint.key())?.amount;
    let a = &ctx.accounts;
    let basket_key = a.basket.key();
    require!(a.basket.flag(STAKER_CUT), VaultError::NotExecutable);
    let usdc = crate::instructions::mint_redeem::USDC_MINT;
    require!(is_ata(&a.treasury_usdc.to_account_info(), &a.protocol.key(), &usdc), VaultError::BadRemainingAccounts);
    let (ext_ai, pot_ai) = (a.ext.to_account_info(), a.pot.to_account_info());
    let pot = pot_balance(&pot_ai, &basket_key)?;
    let mut rec = load_usdc(&basket_key, &ext_ai)?;
    sync_usdc(&mut rec, pot, staked)?;
    let pay = rec.treasury.min(pot);
    rec.treasury -= pay;
    store_usdc(&basket_key, &ext_ai, &rec)?;
    if pay > 0 {
        let (_, bump, _) = pot_of(&basket_key);
        let b = [bump];
        let seeds: [&[u8]; 3] = [crate::instructions::mint_redeem::CUT_POT_SEED, basket_key.as_ref(), &b];
        anchor_spl::token::transfer(
            CpiContext::new_with_signer(
                a.token_program.to_account_info(),
                anchor_spl::token::Transfer { from: pot_ai, to: a.treasury_usdc.to_account_info(), authority: a.pot_authority.to_account_info() },
                &[&seeds],
            ),
            pay,
        )?;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct PotToTreasury<'info> {
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: `[EXT_SEED, basket]`, checked by `ext`.
    #[account(mut)]
    pub ext: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the owner of staked shares.
    #[account(seeds = [SHARE_ESCROW_SEED], bump)]
    pub escrow_authority: UncheckedAccount<'info>,
    /// CHECK: the escrow's canonical account for this share mint, checked by `read_vault`.
    pub escrow: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the pot's owner.
    #[account(seeds = [crate::instructions::mint_redeem::CUT_POT_SEED, basket.key().as_ref()], bump)]
    pub pot_authority: UncheckedAccount<'info>,
    /// CHECK: the pot, checked by `pot_balance`.
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    /// CHECK: the protocol's USDC account, checked in the instruction.
    #[account(mut)]
    pub treasury_usdc: UncheckedAccount<'info>,
    #[account(address = anchor_spl::token::ID)]
    pub token_program: Program<'info, anchor_spl::token::Token>,
}

#[derive(Accounts)]
pub struct ClaimStakeCutUsdc<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    /// CHECK: `[EXT_SEED, basket]`, checked by `ext`.
    #[account(mut)]
    pub ext: UncheckedAccount<'info>,
    /// CHECK: address-only PDA, the owner of staked shares.
    #[account(seeds = [SHARE_ESCROW_SEED], bump)]
    pub escrow_authority: UncheckedAccount<'info>,
    /// CHECK: the escrow's canonical account for this share mint, checked by `read_vault`.
    pub escrow: UncheckedAccount<'info>,
    #[account(mut, seeds = [SHARE_STAKE_SEED, basket.key().as_ref(), wallet.key().as_ref()], bump = share_stake.bump)]
    pub share_stake: Box<Account<'info, ShareStake>>,
    /// CHECK: address-only PDA, the pot's owner.
    #[account(seeds = [crate::instructions::mint_redeem::CUT_POT_SEED, basket.key().as_ref()], bump)]
    pub pot_authority: UncheckedAccount<'info>,
    /// CHECK: the pot, checked by `pot_balance`.
    #[account(mut)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: the wallet's USDC account; the token program checks it as it moves.
    #[account(mut)]
    pub wallet_usdc: UncheckedAccount<'info>,
    #[account(address = anchor_spl::token::ID)]
    pub token_program: Program<'info, anchor_spl::token::Token>,
    pub system_program: Program<'info, System>,
}
