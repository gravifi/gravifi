//! Smart baskets: a basket whose assets and targets are set by the strategist, the app's key that
//! reviews it every day, only within guardrails the creator wrote on its `Smart` account. The
//! strategist can change targets, add and remove assets (adding burns no GRAVI) and put the
//! mandate's stop on an asset. It can't move a token, change a fee or a guardrail, or act on a
//! paused basket. The creator changes the guardrails and the mandate with a day's notice, so
//! holders who disagree can redeem first; the creator or the guardian can pause the basket, which
//! keeps it on its last targets. Stops and rebalancing go on throughout.
//!
//! Mints and redeems of a smart basket pay the Smart-mode protocol fee (`smart_fee_bps`), split
//! between the burn and the treasury as the normal one is.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint as MintI, TokenInterface};

use crate::errors::VaultError;
use crate::instructions::basket::{burn_gravi, check_addable, drop_emptied, GraviAccounts};
use crate::state::*;

#[event]
pub struct SmartTargetsSet {
    pub basket: Pubkey,
    pub targets_before: Vec<u16>,
    pub targets_after: Vec<u16>,
    /// The sum of the increases, in bps.
    pub change_bps: u16,
}

#[event]
pub struct SmartAssetAdded {
    pub basket: Pubkey,
    pub mint: Pubkey,
}

#[event]
pub struct SmartAssetRemoved {
    pub basket: Pubkey,
    pub mint: Pubkey,
}

#[event]
pub struct MandateProposed {
    pub basket: Pubkey,
    pub mandate_hash: [u8; 32],
    pub leave: bool,
    pub applies_at: i64,
}

#[event]
pub struct MandateApplied {
    pub basket: Pubkey,
    pub mandate_hash: [u8; 32],
    pub leave: bool,
}

#[event]
pub struct SmartPauseSet {
    pub basket: Pubkey,
    pub paused_by: u8,
}

// ------------------------------------------------------------------------------- protocol

/// Names the strategist (`Pubkey::default()` removes it) and sets the Smart-mode protocol fee, at
/// most `MAX_SMART_FEE_BPS`. The first call creates the strategist's account. The fee sits in the
/// protocol account's tail (`SMART_FEE_OFFSET`), which grows to hold it the first time.
pub fn set_smart_config(ctx: Context<SetSmartConfig>, strategist: Pubkey, fee_bps: u16) -> Result<()> {
    require!(fee_bps <= MAX_SMART_FEE_BPS, VaultError::FeeTooHigh);
    let s = &mut ctx.accounts.strategist;
    s.bump = ctx.bumps.strategist;
    s.strategist = strategist;

    let ai = ctx.accounts.protocol.to_account_info();
    let need = SMART_FEE_OFFSET + SMART_FEE_LEN;
    if ai.data_len() < need {
        let short = Rent::get()?.minimum_balance(need).saturating_sub(ai.lamports());
        if short > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer { from: ctx.accounts.authority.to_account_info(), to: ai.clone() },
                ),
                short,
            )?;
        }
        ai.resize(need)?;
    }
    let [lo, hi] = fee_bps.to_le_bytes();
    ai.try_borrow_mut_data()?[SMART_FEE_OFFSET..need].copy_from_slice(&[lo, hi, 1]);
    Ok(())
}

/// Sets the GRAVI, in base units, a creator burns once on making a basket smart: 0 turns it off,
/// and it can't go over `MAX_SMART_BURN_TOKENS` whole GRAVI. It sits in the protocol account's
/// tail (`SMART_BURN_OFFSET`), which grows to hold it the first time. Protocol authority only.
pub fn set_smart_burn(ctx: Context<SetSmartBurn>, amount: u64) -> Result<()> {
    let ceiling = 10u64
        .checked_pow(ctx.accounts.gravi_mint.decimals as u32)
        .and_then(|one| one.checked_mul(MAX_SMART_BURN_TOKENS))
        .ok_or(VaultError::MathOverflow)?;
    require!(amount <= ceiling, VaultError::CreationBurnTooHigh);

    let ai = ctx.accounts.protocol.to_account_info();
    let need = SMART_BURN_OFFSET + SMART_BURN_LEN;
    if ai.data_len() < need {
        let short = Rent::get()?.minimum_balance(need).saturating_sub(ai.lamports());
        if short > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer { from: ctx.accounts.authority.to_account_info(), to: ai.clone() },
                ),
                short,
            )?;
        }
        ai.resize(need)?;
    }
    ai.try_borrow_mut_data()?[SMART_BURN_OFFSET..need].copy_from_slice(&amount.to_le_bytes());
    Ok(())
}

/// Marks an asset GraviFi Verified (creates its record) or takes the mark away (closes it, the
/// rent back to the authority). Protocol authority only.
pub fn set_verified(ctx: Context<SetVerified>, verified: bool) -> Result<()> {
    let ai = ctx.accounts.verified.to_account_info();
    let exists = ai.owner == &crate::ID && ai.data_len() > 0;
    let mint = ctx.accounts.mint.key();
    let (expected, bump) = Pubkey::find_program_address(&[VERIFIED_SEED, mint.as_ref()], &crate::ID);
    require_keys_eq!(*ai.key, expected, VaultError::BadVerifiedRecord);
    let authority = ctx.accounts.authority.to_account_info();
    if verified && !exists {
        let space = 8 + VerifiedAsset::INIT_SPACE;
        let seeds: [&[u8]; 3] = [VERIFIED_SEED, mint.as_ref(), &[bump]];
        let short = Rent::get()?.minimum_balance(space).saturating_sub(ai.lamports());
        if short > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer { from: authority.clone(), to: ai.clone() },
                ),
                short,
            )?;
        }
        let system = ctx.accounts.system_program.to_account_info();
        anchor_lang::system_program::allocate(
            CpiContext::new_with_signer(system.clone(), anchor_lang::system_program::Allocate { account_to_allocate: ai.clone() }, &[&seeds]),
            space as u64,
        )?;
        anchor_lang::system_program::assign(
            CpiContext::new_with_signer(system, anchor_lang::system_program::Assign { account_to_assign: ai.clone() }, &[&seeds]),
            &crate::ID,
        )?;
        let record = VerifiedAsset { bump, mint };
        let mut data = ai.try_borrow_mut_data()?;
        let mut out: &mut [u8] = &mut data;
        record.try_serialize(&mut out)?;
    } else if !verified && exists {
        let lamports = ai.lamports();
        **ai.try_borrow_mut_lamports()? = 0;
        **authority.try_borrow_mut_lamports()? = authority.lamports().checked_add(lamports).ok_or(VaultError::MathOverflow)?;
        ai.resize(0)?;
        ai.assign(&anchor_lang::solana_program::system_program::ID);
    }
    Ok(())
}

/// Whether `ai` is `mint`'s Verified record (and it exists). Any other address is refused, so the
/// strategist can't pass something else for it.
pub(crate) fn is_verified(ai: &AccountInfo, mint: &Pubkey) -> Result<bool> {
    let (expected, _) = Pubkey::find_program_address(&[VERIFIED_SEED, mint.as_ref()], &crate::ID);
    require_keys_eq!(*ai.key, expected, VaultError::BadVerifiedRecord);
    Ok(ai.owner == &crate::ID && ai.data_len() > 0)
}

// -------------------------------------------------------------------------------- creator

/// Ranges a creator's guardrails must fall in: never looser than the code's ceilings, and
/// satisfiable (enough assets at the weight cap to fill the basket).
pub(crate) fn check_guardrails(g: &Guardrails, mandate_hash: &[u8; 32]) -> Result<()> {
    let ok = *mandate_hash != [0u8; 32]
        && (1..=SMART_MAX_DAY_BPS).contains(&g.max_day_bps)
        && (g.max_day_bps..=SMART_MAX_WEEK_BPS).contains(&g.max_week_bps)
        && (1..=SMART_MAX_WEIGHT_BPS).contains(&g.max_weight_bps)
        && g.max_unverified_bps <= SMART_MAX_UNVERIFIED_BPS.min(g.max_weight_bps)
        && g.min_assets >= 2
        && g.min_assets <= g.max_assets
        && g.max_assets as usize <= MAX_LAUNCH_CONSTITUENTS
        && g.max_weight_bps as u32 * g.max_assets as u32 >= BPS_DENOM as u32
        && (g.stop_bps == 0 || (MIN_STOP_BPS..=MAX_STOP_BPS).contains(&g.stop_bps));
    require!(ok, VaultError::BadGuardrails);
    Ok(())
}

/// Makes a creator-managed basket smart, before its first deposit: from then on only the
/// strategist sets its targets and assets, within `guard`, and its mints and redeems pay the
/// Smart-mode fee. The creator pays the rules account's rent and burns the smart burn
/// (`set_smart_burn`) from their GRAVI, unless they're the burn-exempt wallet.
pub fn enable_smart(ctx: Context<EnableSmart>, mandate_hash: [u8; 32], guard: Guardrails) -> Result<()> {
    require!(ctx.accounts.basket.control == ControlMode::Creator && !ctx.accounts.basket.seeded, VaultError::SmartNotAllowed);
    check_guardrails(&guard, &mandate_hash)?;

    let a = &mut *ctx.accounts;
    let protocol = a.protocol.to_account_info();
    burn_gravi(
        &mut a.creation_burn,
        &a.protocol,
        GraviAccounts { mint: a.gravi_mint.as_ref(), from: a.creator_gravi.as_ref(), program: a.gravi_token_program.as_ref() },
        a.creator.to_account_info(),
        smart_burn(&protocol),
        false,
        burn_exempt(&protocol),
    )?;

    let basket = &mut ctx.accounts.basket;
    basket.control = ControlMode::Smart;

    let s = &mut ctx.accounts.smart;
    s.bump = ctx.bumps.smart;
    s.basket = basket.key();
    s.mandate_hash = mandate_hash;
    s.guard = guard;
    s.paused_by = 0;
    s.pending_at = 0;
    s.pending_hash = [0; 32];
    s.pending_guard = Guardrails::default();
    s.pending_leave = false;
    s.hour = Clock::get()?.unix_timestamp / 3_600;
    s.turnover = [0; TURNOVER_HOURS];
    s.changes = 0;
    Ok(())
}

/// Proposes a new mandate and guardrails, or (`leave`) handing the basket back to the creator's
/// own control. Either applies a day from now (`apply_mandate`); proposing again replaces the
/// waiting one and starts the day again, and an all-zero hash (not leaving) withdraws it.
/// Creator only.
pub fn propose_mandate(ctx: Context<CreatorSmart>, mandate_hash: [u8; 32], guard: Guardrails, leave: bool) -> Result<()> {
    if !leave && mandate_hash == [0u8; 32] {
        let s = &mut ctx.accounts.smart;
        require!(s.pending_at != 0, VaultError::NoPendingMandate);
        s.pending_at = 0;
        s.pending_hash = [0; 32];
        s.pending_guard = Guardrails::default();
        s.pending_leave = false;
        return Ok(());
    }
    if !leave {
        check_guardrails(&guard, &mandate_hash)?;
    }
    let applies_at = Clock::get()?.unix_timestamp + MANDATE_NOTICE_SECS;
    let s = &mut ctx.accounts.smart;
    s.pending_at = applies_at;
    s.pending_hash = if leave { [0; 32] } else { mandate_hash };
    s.pending_guard = if leave { Guardrails::default() } else { guard };
    s.pending_leave = leave;
    emit!(MandateProposed { basket: s.basket, mandate_hash: s.pending_hash, leave, applies_at });
    Ok(())
}

/// Applies the waiting mandate once its day of notice is over. Permissionless. Leaving returns
/// the basket to creator control and closes its rules account, the rent back to the creator.
pub fn apply_mandate(ctx: Context<ApplyMandate>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let s = &mut ctx.accounts.smart;
    require!(s.pending_at != 0, VaultError::NoPendingMandate);
    require!(now >= s.pending_at, VaultError::MandateNoticeRunning);
    let basket_key = s.basket;
    if s.pending_leave {
        ctx.accounts.basket.control = ControlMode::Creator;
        emit!(MandateApplied { basket: basket_key, mandate_hash: [0; 32], leave: true });
        return ctx.accounts.smart.close(ctx.accounts.creator.to_account_info());
    }
    s.mandate_hash = s.pending_hash;
    s.guard = s.pending_guard;
    s.pending_at = 0;
    s.pending_hash = [0; 32];
    s.pending_guard = Guardrails::default();
    emit!(MandateApplied { basket: basket_key, mandate_hash: s.mandate_hash, leave: false });
    Ok(())
}

/// Pauses or resumes the strategist on this basket. The creator or the guardian may pause; a
/// pause is lifted by whoever made it, or by the guardian.
pub fn set_smart_pause(ctx: Context<SetSmartPause>, paused: bool) -> Result<()> {
    let signer = ctx.accounts.signer.key();
    let is_creator = signer == ctx.accounts.basket.creator;
    let guardian = ctx.accounts.protocol.guardian;
    let is_guardian = guardian != Pubkey::default() && signer == guardian;
    require!(is_creator || is_guardian, VaultError::NotPauser);
    let s = &mut ctx.accounts.smart;
    if paused {
        s.paused_by = if is_creator { PAUSED_BY_CREATOR } else { PAUSED_BY_GUARDIAN };
    } else {
        require!(is_guardian || s.paused_by != PAUSED_BY_GUARDIAN, VaultError::NotPauser);
        s.paused_by = 0;
    }
    emit!(SmartPauseSet { basket: s.basket, paused_by: s.paused_by });
    Ok(())
}

// ----------------------------------------------------------------------------- strategist

fn check_running(basket: &Basket, smart: &Smart) -> Result<()> {
    require!(basket.control == ControlMode::Smart, VaultError::NotSmartBasket);
    require!(smart.paused_by == 0, VaultError::SmartPaused);
    Ok(())
}

/// Sets a smart basket's targets, within its guardrails. Remaining accounts: each constituent's
/// Verified record address (`[VERIFIED_SEED, mint]`, existing or not), in order.
///
/// - no asset over the weight cap (the Unverified cap without a record), unless it's lowered;
/// - no more assets above 0% than the maximum, and no fewer than the minimum unless that's no
///   fewer than now (a stop may have left it short);
/// - the change, the sum of the increases, within what's left of the 24-hour and 7-day caps.
pub fn smart_set_targets<'info>(ctx: Context<'_, '_, 'info, 'info, StrategistBasket<'info>>, target_bps: Vec<u16>) -> Result<()> {
    check_running(&ctx.accounts.basket, &ctx.accounts.smart)?;
    let basket = &mut ctx.accounts.basket;
    let s = &mut ctx.accounts.smart;
    let g = s.guard;
    let n = basket.constituent_count as usize;
    require!(target_bps.len() == n, VaultError::BadRemainingAccounts);
    require!(ctx.remaining_accounts.len() == n, VaultError::BadRemainingAccounts);
    let sum: u32 = target_bps.iter().map(|b| *b as u32).sum();
    require!(sum == BPS_DENOM as u32, VaultError::WeightsNotNormalised);

    let before: Vec<u16> = basket.active().iter().map(|c| c.target_bps).collect();
    let mut change: u32 = 0;
    for i in 0..n {
        let (old, new) = (before[i], target_bps[i]);
        let cap = if is_verified(&ctx.remaining_accounts[i], &basket.constituents[i].mint)? { g.max_weight_bps } else { g.max_unverified_bps };
        require!(new <= cap || new <= old, VaultError::WeightAboveCap);
        change += new.saturating_sub(old) as u32;
    }
    let held_now = before.iter().filter(|t| **t > 0).count();
    let held = target_bps.iter().filter(|t| **t > 0).count();
    require!(held <= g.max_assets as usize, VaultError::AssetCountOutOfRange);
    require!(held >= g.min_assets as usize || held >= held_now, VaultError::AssetCountOutOfRange);

    s.advance(Clock::get()?.unix_timestamp / 3_600);
    let (day, week) = s.used();
    require!(day + change <= g.max_day_bps as u32, VaultError::TurnoverDayCap);
    require!(week + change <= g.max_week_bps as u32, VaultError::TurnoverWeekCap);
    let k = s.hour.rem_euclid(TURNOVER_HOURS as i64) as usize;
    // Fits: one hour's change is at most the day's cap.
    s.turnover[k] += change as u16;
    s.changes = s.changes.saturating_add(1);

    for i in 0..n {
        basket.constituents[i].target_bps = target_bps[i];
    }
    emit!(SmartTargetsSet { basket: basket.key(), targets_before: before, targets_after: target_bps, change_bps: change as u16 });
    Ok(())
}

/// Adds a listed asset to a smart basket at 0%, as `add_constituent` does, but burns no GRAVI:
/// assets the strategist finds are free to add. (Those the creator picks at creation still
/// burn.) The basket's vault for it must exist.
pub fn smart_add_constituent(ctx: Context<StrategistAdd>) -> Result<()> {
    check_running(&ctx.accounts.basket, &ctx.accounts.smart)?;
    check_addable(
        &ctx.accounts.basket,
        &ctx.accounts.mint.to_account_info(),
        &ctx.accounts.asset_entry.to_account_info(),
        &ctx.accounts.vault.to_account_info(),
    )?;
    let mint = ctx.accounts.mint.key();
    let basket = &mut ctx.accounts.basket;
    basket.constituents.push(Constituent { mint, target_bps: 0, units_per_share: 0 });
    basket.constituent_count += 1;
    emit!(SmartAssetAdded { basket: basket.key(), mint });
    Ok(())
}

/// Drops an asset the smart basket has sold out (0% target, no units), as `remove_constituent`.
pub fn smart_remove_constituent(ctx: Context<StrategistBasket>, index: u8) -> Result<()> {
    check_running(&ctx.accounts.basket, &ctx.accounts.smart)?;
    let basket = &mut ctx.accounts.basket;
    let mint = basket.constituents.get(index as usize).map(|c| c.mint).unwrap_or_default();
    drop_emptied(basket, index)?;
    emit!(SmartAssetRemoved { basket: basket.key(), mint });
    Ok(())
}

/// Puts the mandate's stop (`guard.stop_bps`, trailing or not) on one of the basket's assets that
/// has none, below its fresh posted price. Stops on assets no longer in the basket are dropped
/// first. The strategist pays the stop account's rent the first time.
pub fn smart_set_stop(ctx: Context<StrategistStop>) -> Result<()> {
    check_running(&ctx.accounts.basket, &ctx.accounts.smart)?;
    let g = ctx.accounts.smart.guard;
    require!(g.stop_bps != 0, VaultError::SmartStopNotAllowed);
    let now = Clock::get()?.unix_timestamp;
    let price = &ctx.accounts.asset_price;
    require!(price.price_e12 > 0 && now - price.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
    let basket = &ctx.accounts.basket;
    require!(basket.active().iter().any(|c| c.mint == price.mint), VaultError::BadStop);

    let st = &mut ctx.accounts.stop_loss;
    st.bump = ctx.bumps.stop_loss;
    st.basket = basket.key();
    st.stops.retain(|s| basket.active().iter().any(|c| c.mint == s.mint));
    require!(st.stops.iter().all(|s| s.mint != price.mint), VaultError::SmartStopNotAllowed);
    st.stops.push(Stop { mint: price.mint, pct_bps: g.stop_bps, trailing: g.stop_trailing, reference_e12: price.price_e12, armed_at: 0 });
    Ok(())
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct SetSmartConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// Grown by hand to hold the fee after its fields (see SMART_FEE_OFFSET).
    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    #[account(init_if_needed, payer = authority, space = 8 + Strategist::INIT_SPACE, seeds = [STRATEGIST_SEED], bump)]
    pub strategist: Account<'info, Strategist>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetVerified<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    /// CHECK: any mint; only its address is recorded.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: `[VERIFIED_SEED, mint]`, created or closed by hand in `set_verified`.
    #[account(mut)]
    pub verified: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct EnableSmart<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(mut, has_one = creator @ VaultError::NotMandateAuthority)]
    pub basket: Account<'info, Basket>,
    #[account(init, payer = creator, space = 8 + Smart::INIT_SPACE, seeds = [SMART_SEED, basket.key().as_ref()], bump)]
    pub smart: Box<Account<'info, Smart>>,
    pub system_program: Program<'info, System>,
    /// Read for the smart burn and the burn-exempt wallet, in its tail.
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    /// Counts the smart burn with the creation burn.
    #[account(mut, seeds = [CREATION_BURN_SEED], bump = creation_burn.bump)]
    pub creation_burn: Box<Account<'info, CreationBurn>>,
    /// CHECK: GRAVI, matched against the protocol's mint in `burn_gravi`. Needed only while the
    /// smart burn is on.
    #[account(mut)]
    pub gravi_mint: Option<UncheckedAccount<'info>>,
    /// CHECK: the creator's GRAVI account; the token program checks it as it burns.
    #[account(mut)]
    pub creator_gravi: Option<UncheckedAccount<'info>>,
    pub gravi_token_program: Option<Interface<'info, TokenInterface>>,
}

#[derive(Accounts)]
pub struct SetSmartBurn<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// Grown by hand to hold the smart burn after its fields (see SMART_BURN_OFFSET).
    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    /// Read for its decimals, to set the ceiling in whole GRAVI. Fails until the mint is set.
    #[account(address = protocol.gravi_mint @ VaultError::GraviMintNotSet)]
    pub gravi_mint: InterfaceAccount<'info, MintI>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreatorSmart<'info> {
    pub creator: Signer<'info>,
    #[account(has_one = creator @ VaultError::NotMandateAuthority)]
    pub basket: Account<'info, Basket>,
    #[account(mut, seeds = [SMART_SEED, basket.key().as_ref()], bump = smart.bump, has_one = basket)]
    pub smart: Box<Account<'info, Smart>>,
}

#[derive(Accounts)]
pub struct ApplyMandate<'info> {
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    #[account(mut, seeds = [SMART_SEED, basket.key().as_ref()], bump = smart.bump, has_one = basket)]
    pub smart: Box<Account<'info, Smart>>,
    /// CHECK: the basket's creator, who gets the rules account's rent back on leaving.
    #[account(mut, address = basket.creator @ VaultError::NotMandateAuthority)]
    pub creator: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetSmartPause<'info> {
    pub signer: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    pub basket: Account<'info, Basket>,
    #[account(mut, seeds = [SMART_SEED, basket.key().as_ref()], bump = smart.bump, has_one = basket)]
    pub smart: Box<Account<'info, Smart>>,
}

#[derive(Accounts)]
pub struct StrategistBasket<'info> {
    pub strategist: Signer<'info>,
    #[account(seeds = [STRATEGIST_SEED], bump = strategist_config.bump, constraint = strategist_config.strategist == strategist.key() @ VaultError::NotStrategist)]
    pub strategist_config: Account<'info, Strategist>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    #[account(mut, seeds = [SMART_SEED, basket.key().as_ref()], bump = smart.bump, has_one = basket)]
    pub smart: Box<Account<'info, Smart>>,
}

#[derive(Accounts)]
pub struct StrategistAdd<'info> {
    pub strategist: Signer<'info>,
    #[account(seeds = [STRATEGIST_SEED], bump = strategist_config.bump, constraint = strategist_config.strategist == strategist.key() @ VaultError::NotStrategist)]
    pub strategist_config: Account<'info, Strategist>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    #[account(seeds = [SMART_SEED, basket.key().as_ref()], bump = smart.bump, has_one = basket)]
    pub smart: Box<Account<'info, Smart>>,
    /// CHECK: the asset's mint, matched against its listing entry.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: the asset's listing entry, read by `load_asset_entry`.
    pub asset_entry: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for the asset, checked by `read_vault` (canonical ATA).
    pub vault: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct StrategistStop<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,
    #[account(seeds = [STRATEGIST_SEED], bump = strategist_config.bump, constraint = strategist_config.strategist == strategist.key() @ VaultError::NotStrategist)]
    pub strategist_config: Account<'info, Strategist>,
    pub basket: Account<'info, Basket>,
    #[account(seeds = [SMART_SEED, basket.key().as_ref()], bump = smart.bump, has_one = basket)]
    pub smart: Box<Account<'info, Smart>>,
    #[account(init_if_needed, payer = strategist, space = 8 + StopLoss::INIT_SPACE, seeds = [STOP_SEED, basket.key().as_ref()], bump)]
    pub stop_loss: Box<Account<'info, StopLoss>>,
    #[account(seeds = [PRICE_SEED, asset_price.mint.as_ref()], bump = asset_price.bump)]
    pub asset_price: Account<'info, AssetPrice>,
    pub system_program: Program<'info, System>,
}
