//! Closing an emptied basket (the team, 9 Nov 2026: a creator's rent otherwise stays locked for good).
//! Once every share is gone (supply 0), the basket's manager takes back the rent of what the
//! program can close, in two steps so each transaction stays small:
//!
//! 1. `close_vaults`, as often as needed: each named token account the basket owns (its vaults,
//!    and any token it was sent) has what's left in it sent to the manager (with no shares out,
//!    only the manager could reopen the basket and so own it anyway) and is closed, its rent to the
//!    manager.
//! 2. `close_basket`, once no vault of its assets is left: the basket account, and those of its
//!    side accounts named (extension, stops, smart rules, cut notice, rebalance state, community
//!    state, allowlist entries, the treasury's and the burn's empty share accounts), rent to the
//!    manager.
//!
//! Not while a trip is out or stuck, nor with a proposal open or anything owed. The share mint was
//! made without a close authority, and its Metaplex metadata has no close: those two keep their rent.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI};

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;

#[event]
pub struct BasketClosed {
    pub basket: Pubkey,
    pub manager: Pubkey,
    pub lamports: u64,
}

fn check_closable(basket: &Basket, share_mint: &InterfaceAccount<MintI>, manager: &Pubkey) -> Result<()> {
    require_keys_eq!(*manager, basket.manager(), VaultError::NotMandateAuthority);
    require!(share_mint.supply == 0, VaultError::BasketNotEmpty);
    require!(basket.creator_unclaimed == 0 && basket.stake_owed == 0, VaultError::BasketNotEmpty);
    require!(!basket.flag(TRIP_OPEN) && !basket.flag(TRIP_STUCK), VaultError::TripOpen);
    Ok(())
}

/// Empties and closes token accounts the basket owns, the rent and what's left to the manager.
/// Remaining accounts, per account: [token account (the basket's canonical one for its mint), the
/// manager's token account for that mint, the mint, its token program]. A Token-2022 account with
/// transfer fees withheld must have them harvested to the mint first (anyone may).
pub fn close_vaults<'info>(ctx: Context<'_, '_, 'info, 'info, CloseVaults<'info>>) -> Result<()> {
    check_closable(&ctx.accounts.basket, &ctx.accounts.share_mint, &ctx.accounts.manager.key())?;
    let rem = ctx.remaining_accounts;
    require!(!rem.is_empty() && rem.len() % 4 == 0, VaultError::BadRemainingAccounts);
    let basket = &ctx.accounts.basket;
    let basket_key = basket.key();
    let creator = basket.creator;
    let basket_id = basket.basket_id;
    let bump = [basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let manager = ctx.accounts.manager.key();
    for q in rem.chunks(4) {
        let (vault, to, mint, program) = (&q[0], &q[1], &q[2], &q[3]);
        // The vault's own token program, so the basket's signature goes to no other program.
        require!(vault.owner == program.key && mint.owner == program.key, VaultError::ConstituentMismatch);
        let held = read_vault(vault, &basket_key, mint.key)?.amount;
        if held > 0 {
            let dest = read_token_account(to)?;
            require_keys_eq!(dest.owner, manager, VaultError::NotMandateAuthority);
            require_keys_eq!(dest.mint, *mint.key, VaultError::ConstituentMismatch);
            let decimals = mint_decimals(mint)?;
            transfer_from_vault(mint, vault, to, &ctx.accounts.basket.to_account_info(), program, held, decimals, &[&seeds])?;
        }
        token_interface::close_account(CpiContext::new_with_signer(
            program.clone(),
            token_interface::CloseAccount { account: vault.clone(), destination: ctx.accounts.manager.to_account_info(), authority: ctx.accounts.basket.to_account_info() },
            &[&seeds],
        ))?;
    }
    Ok(())
}

/// Closes a program-owned account at `ai` (if it's there), its lamports to `to`.
fn close_owned(ai: &AccountInfo, to: &AccountInfo) -> Result<u64> {
    if ai.owner != &crate::ID || ai.data_len() == 0 {
        return Ok(0);
    }
    let lamports = ai.lamports();
    **ai.try_borrow_mut_lamports()? = 0;
    **to.try_borrow_mut_lamports()? = to.lamports().checked_add(lamports).ok_or(VaultError::MathOverflow)?;
    ai.resize(0)?;
    ai.assign(&anchor_lang::solana_program::system_program::ID);
    Ok(lamports)
}

/// Closes the emptied basket and the side accounts named, their rent to the manager. Remaining
/// accounts: first each asset's vault address (`constituent_count` of them, in order: each must be
/// closed already), then any of the basket's own side accounts, found by address: its extension
/// account, stops, smart rules, cut notice, rebalance state, community state, allowlist entries.
/// Any other account is refused.
pub fn close_basket<'info>(ctx: Context<'_, '_, 'info, 'info, CloseBasket<'info>>) -> Result<()> {
    check_closable(&ctx.accounts.basket, &ctx.accounts.share_mint, &ctx.accounts.manager.key())?;
    let basket = &ctx.accounts.basket;
    let basket_key = basket.key();
    let n = basket.constituent_count as usize;
    let rem = ctx.remaining_accounts;
    require!(rem.len() >= n, VaultError::BadRemainingAccounts);
    // Every asset's vault is gone (close_vaults): nothing of the basket's is left in one.
    for (i, c) in basket.active().iter().enumerate() {
        // The vault under either token program: with no account there, which one the mint uses
        // can't be read from the vault, and neither address may hold one.
        let ai = &rem[i];
        let closed = ai.data_len() == 0 && ai.owner == &anchor_lang::solana_program::system_program::ID;
        let at = |program: &Pubkey| Pubkey::find_program_address(&[basket_key.as_ref(), program.as_ref(), c.mint.as_ref()], &ATA_PROGRAM_ID).0;
        let named = *ai.key == at(&anchor_spl::token::ID) || *ai.key == at(&anchor_spl::token_2022::ID);
        require!(named, VaultError::BadRemainingAccounts);
        require!(closed, VaultError::VaultsOpen);
    }

    let manager = ctx.accounts.manager.to_account_info();
    let pda = |seed: &[u8]| Pubkey::find_program_address(&[seed, basket_key.as_ref()], &crate::ID).0;
    let side = [pda(crate::ext::EXT_SEED), pda(STOP_SEED), pda(SMART_SEED), pda(CUT_NOTICE_SEED), pda(REBALANCE_SEED), pda(COMMUNITY_SEED)];
    let community = side[5];
    // A basket that's been vote-run has a USDC pot for its share stakers: it must be named (with its
    // owner and the SPL Token program) and owe them no more than dust (shared out but unclaimed: with
    // no shares out nobody is staked, so what's never been shared, and anything sent there, is
    // nobody's). What's left goes to a USDC account of the manager's, named too, and the pot closes.
    let (pot_owner, pot_bump, pot) = crate::instructions::stake_cut::pot_of(&basket_key);
    let pot_ai = rem[n..].iter().find(|a| *a.key == pot);
    let mut pot_left = 0u64;
    if basket.flag(STAKER_CUT) {
        let ai = pot_ai.ok_or(VaultError::BadPot)?;
        pot_left = crate::instructions::stake_cut::pot_balance(ai, &basket_key)?;
        if pot_left > 0 {
            let ext_ai = rem[n..].iter().find(|a| *a.key == side[0]).ok_or(VaultError::BadExt)?;
            // Stakers' USDC and the treasury's (`pot_to_treasury` first) leave only dust to sweep.
            let rec = crate::instructions::stake_cut::load_usdc(&basket_key, ext_ai)?;
            require!(rec.allocated.saturating_add(rec.treasury) <= POT_DUST_USDC, VaultError::BasketNotEmpty);
        }
    }
    let usdc = crate::instructions::mint_redeem::USDC_MINT;
    let manager_key = basket.manager();
    let to_manager = |ai: &AccountInfo| movable_balance(ai, &usdc, &manager_key).is_some();
    let pot_to = rem[n..].iter().find(|a| to_manager(a));
    require!(pot_left == 0 || pot_to.is_some(), VaultError::BadPot);
    for ai in &rem[n..] {
        let pot_side = *ai.key == pot || *ai.key == pot_owner || *ai.key == anchor_spl::token::ID || pot_to.is_some_and(|t| t.key == ai.key);
        let allowed = side.contains(ai.key) || pot_side || is_allow_entry(ai, &basket_key)?;
        require!(allowed, VaultError::BadRemainingAccounts);
        if *ai.key == community && ai.owner == &crate::ID {
            // Proposals' rent is their proposers', reclaimed through the community state: none open.
            let c = Community::try_deserialize(&mut &ai.try_borrow_data()?[..])?;
            require!(c.open == 0, VaultError::BasketBusy);
        }
    }
    let mut lamports: u64 = 0;

    // The treasury's and the burn's share accounts for this mint, empty with no shares out.
    let a = &ctx.accounts;
    require_keys_eq!(*a.share_token_program.key, *a.share_mint.to_account_info().owner, VaultError::ConstituentMismatch);
    for (acct, owner, seed) in [(&a.treasury_shares, &a.protocol.to_account_info(), PROTOCOL_SEED), (&a.burn_shares, &a.burn_authority.to_account_info(), BURN_SEED)] {
        let Some(acct) = acct.as_ref() else { continue };
        let ai = acct.to_account_info();
        if ai.data_len() == 0 {
            continue;
        }
        let view = read_token_account(&ai)?;
        require!(ai.owner == a.share_token_program.key, VaultError::ConstituentMismatch);
        require!(view.owner == *owner.key && view.mint == a.share_mint.key() && view.amount == 0, VaultError::BadRemainingAccounts);
        let (_, bump) = Pubkey::find_program_address(&[seed], &crate::ID);
        let b = [bump];
        let seeds: [&[u8]; 2] = [seed, &b];
        lamports = lamports.checked_add(ai.lamports()).ok_or(VaultError::MathOverflow)?;
        token_interface::close_account(CpiContext::new_with_signer(
            a.share_token_program.to_account_info(),
            token_interface::CloseAccount { account: ai.clone(), destination: manager.clone(), authority: owner.clone() },
            &[&seeds],
        ))?;
    }

    // The pot, empty (checked above), if it was ever created.
    if let Some(pot_ai) = pot_ai.filter(|ai| ai.data_len() > 0) {
        let owner_ai = rem[n..].iter().find(|a| *a.key == pot_owner).ok_or(VaultError::BadPot)?;
        let program_ai = rem[n..].iter().find(|a| *a.key == anchor_spl::token::ID).ok_or(VaultError::BadPot)?;
        let b = [pot_bump];
        let seeds: [&[u8]; 3] = [crate::instructions::mint_redeem::CUT_POT_SEED, basket_key.as_ref(), &b];
        if pot_left > 0 {
            anchor_spl::token::transfer(
                CpiContext::new_with_signer(program_ai.clone(), anchor_spl::token::Transfer { from: pot_ai.clone(), to: pot_to.unwrap().clone(), authority: owner_ai.clone() }, &[&seeds]),
                pot_left,
            )?;
        }
        lamports = lamports.checked_add(pot_ai.lamports()).ok_or(VaultError::MathOverflow)?;
        token_interface::close_account(CpiContext::new_with_signer(
            program_ai.clone(),
            token_interface::CloseAccount { account: pot_ai.clone(), destination: manager.clone(), authority: owner_ai.clone() },
            &[&seeds],
        ))?;
    }

    // The side accounts after the token program's closes: lamports moved by hand before a CPI
    // would unbalance it.
    for ai in &rem[n..] {
        lamports = lamports.checked_add(close_owned(ai, &manager)?).ok_or(VaultError::MathOverflow)?;
    }

    lamports = lamports.checked_add(ctx.accounts.basket.to_account_info().lamports()).ok_or(VaultError::MathOverflow)?;
    emit!(BasketClosed { basket: basket_key, manager: manager.key(), lamports });
    // The basket account itself: `close = manager` below.
    Ok(())
}

/// Whether `ai` is an allowlist entry of this basket (`[b"allow", basket, wallet]`).
fn is_allow_entry(ai: &AccountInfo, basket: &Pubkey) -> Result<bool> {
    if ai.owner != &crate::ID || ai.data_len() < 8 + AllowEntry::INIT_SPACE {
        return Ok(false);
    }
    let Ok(e) = AllowEntry::try_deserialize(&mut &ai.try_borrow_data()?[..]) else { return Ok(false) };
    let (expected, _) = Pubkey::find_program_address(&[b"allow", basket.as_ref(), e.wallet.as_ref()], &crate::ID);
    Ok(e.basket == *basket && expected == *ai.key)
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct CloseVaults<'info> {
    #[account(mut)]
    pub manager: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
}

#[derive(Accounts)]
pub struct CloseBasket<'info> {
    #[account(mut)]
    pub manager: Signer<'info>,
    #[account(mut, close = manager, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,
    /// CHECK: address-only PDA, the owner of the burn's share accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    /// CHECK: the treasury's share account for this mint, if it exists: checked in the instruction.
    #[account(mut)]
    pub treasury_shares: Option<UncheckedAccount<'info>>,
    /// CHECK: the burn's share account for this mint, if it exists: checked in the instruction.
    #[account(mut)]
    pub burn_shares: Option<UncheckedAccount<'info>>,
    /// CHECK: the share mint's token program, matched by the token program as it closes.
    pub share_token_program: UncheckedAccount<'info>,
}
