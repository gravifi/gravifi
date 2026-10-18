//! Creating a basket and changing it: its recipe, policy, allowlist and splits. Every change
//! goes through `authorise_mandate`, so an immutable basket refuses all of them.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenInterface;

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;
use anchor_lang::solana_program::program::{invoke, invoke_signed};
use anchor_lang::system_program;
use anchor_spl::token_2022::spl_token_2022;

use crate::extensions;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitBasketArgs {
    pub basket_id: [u8; 16],
    pub access: AccessMode,
    /// Creator or Immutable. Holder governance (Dao) has been removed.
    pub control: ControlMode,
    pub rebalance_mode: RebalanceMode,
    pub interval_secs: i64,
    pub drift_bps: u16,
    pub mgmt_fee_bps: u16,
    pub mints: Vec<Pubkey>,
    pub target_bps: Vec<u16>,
}

/// Creates the basket and its share mint. Remaining accounts are one
/// `[mint, asset_entry]` pair per constituent, in the order of `args.mints`.
pub fn initialize_basket<'info>(
    ctx: Context<'_, '_, 'info, 'info, InitializeBasket<'info>>,
    args: InitBasketArgs,
) -> Result<()> {
    let n = args.mints.len();
    require!(n == args.target_bps.len(), VaultError::BadRemainingAccounts);
    require!(n >= 2, VaultError::TooFewConstituents);
    require!(n <= MAX_LAUNCH_CONSTITUENTS, VaultError::TooManyConstituents);
    require!(args.mgmt_fee_bps <= MAX_CREATOR_FEE_BPS, VaultError::FeeTooHigh);
    check_policy(args.rebalance_mode, args.interval_secs, args.drift_bps)?;
    require!(args.control != ControlMode::Dao, VaultError::GovernanceRemoved);
    require!(
        ctx.remaining_accounts.len() == n * 2,
        VaultError::BadRemainingAccounts
    );

    let sum: u32 = args.target_bps.iter().map(|b| *b as u32).sum();
    require!(sum == BPS_DENOM as u32, VaultError::WeightsNotNormalised);

    for i in 0..n {
        for j in (i + 1)..n {
            require!(args.mints[i] != args.mints[j], VaultError::DuplicateConstituent);
        }
    }

    let mut constituents = Vec::with_capacity(n);
    for (i, mint_key) in args.mints.iter().enumerate() {
        let mint_ai = &ctx.remaining_accounts[i * 2];
        let entry_ai = &ctx.remaining_accounts[i * 2 + 1];
        require_keys_eq!(*mint_ai.key, *mint_key, VaultError::ConstituentMismatch);

        let entry = load_asset_entry(entry_ai)?;
        require_keys_eq!(entry.mint, *mint_key, VaultError::AssetNotListed);
        require!(entry.status == AssetStatus::Listed, VaultError::AssetNotListed);

        // Checked again, not just at listing: a hook authority can enable the hook later,
        // and a fee authority can raise the fee.
        assert_no_active_transfer_hook(mint_ai)?;
        TransferFee::of(mint_ai)?.assert_within_cap()?;

        constituents.push(Constituent {
            mint: *mint_key,
            target_bps: args.target_bps[i],
            units_per_share: 0,
        });
    }

    create_share_mint(&ctx)?;

    let now = Clock::get()?.unix_timestamp;
    let b = &mut ctx.accounts.basket;
    b.bump = ctx.bumps.basket;
    b.creator = ctx.accounts.creator.key();
    b.share_mint = ctx.accounts.share_mint.key();
    b.rebalance_supply = 0;
    b.reserved = [0; 24];
    b.basket_id = args.basket_id;
    b.access = args.access;
    b.control = args.control;
    b.rebalance_mode = args.rebalance_mode;
    b.interval_secs = args.interval_secs;
    b.drift_bps = args.drift_bps;
    b.last_rebalance = now;
    b.mgmt_fee_bps = args.mgmt_fee_bps;
    b.last_fee_accrual = now;
    b.seeded = false;
    b.mint_paused = false;
    b.constituent_count = n as u8;
    b.constituents = constituents;

    Ok(())
}

/// Replace the target weights. `Immutable` rejects unconditionally, which is the whole
/// promise of that mode.
pub fn set_targets(ctx: Context<Reconfigure>, target_bps: Vec<u16>) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.authority)?;
    let count = ctx.accounts.basket.constituent_count as usize;
    require!(target_bps.len() == count, VaultError::BadRemainingAccounts);
    let sum: u32 = target_bps.iter().map(|b| *b as u32).sum();
    require!(sum == BPS_DENOM as u32, VaultError::WeightsNotNormalised);

    let basket = &mut ctx.accounts.basket;
    for i in 0..count {
        basket.constituents[i].target_bps = target_bps[i];
    }
    Ok(())
}

/// Adds a listed asset to a creator-managed basket at a 0% target and no units: nothing is
/// deposited or moved. A `set_targets` (normally in the same transaction) gives it a weight,
/// and rebalancing then buys it in, slice by slice, like any underweight sleeve.
///
/// The basket's vault for the asset (its associated token account) must already exist,
/// so every instruction that reads all the vaults keeps working.
pub fn add_constituent(ctx: Context<AddConstituent>) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.authority)?;
    let basket_key = ctx.accounts.basket.key();
    let mint_ai = ctx.accounts.mint.to_account_info();
    let mint_key = mint_ai.key();
    let basket = &ctx.accounts.basket;
    require!((basket.constituent_count as usize) < MAX_LAUNCH_CONSTITUENTS, VaultError::TooManyConstituents);
    require!(basket.active().iter().all(|c| c.mint != mint_key), VaultError::DuplicateConstituent);

    let entry = load_asset_entry(&ctx.accounts.asset_entry.to_account_info())?;
    require_keys_eq!(entry.mint, mint_key, VaultError::AssetNotListed);
    require!(entry.status == AssetStatus::Listed, VaultError::AssetNotListed);
    assert_no_active_transfer_hook(&mint_ai)?;
    TransferFee::of(&mint_ai)?.assert_within_cap()?;
    read_vault(&ctx.accounts.vault.to_account_info(), &basket_key, &mint_key)?;

    let basket = &mut ctx.accounts.basket;
    basket.constituents.push(Constituent { mint: mint_key, target_bps: 0, units_per_share: 0 });
    basket.constituent_count += 1;
    Ok(())
}

/// Drops an asset the basket no longer holds: its target must be 0% and rebalancing must
/// have sold it out (no units per share), so no holder's claim changes. Any dust left in
/// its vault, worth less than one base unit a share, stays there.
pub fn remove_constituent(ctx: Context<Reconfigure>, index: u8) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.authority)?;
    let basket = &mut ctx.accounts.basket;
    let (i, n) = (index as usize, basket.constituent_count as usize);
    require!(i < n, VaultError::BadParameter);
    require!(n > 2, VaultError::TooFewConstituents);
    let c = basket.constituents[i];
    require!(c.target_bps == 0 && c.units_per_share == 0, VaultError::ConstituentNotEmpty);
    basket.constituents.remove(i);
    basket.constituent_count -= 1;
    Ok(())
}

/// Replace the rebalancing cadence and/or the creator fee.
///
/// A new fee applies from now on, never to the time before: changing it restarts the fee
/// clock, which forfeits whatever the old rate had streamed since the last accrual. To collect
/// that first, the creator runs `accrue_fees` earlier in the same transaction (the app does).
pub fn set_policy(
    ctx: Context<Reconfigure>,
    rebalance_mode: RebalanceMode,
    interval_secs: i64,
    drift_bps: u16,
    mgmt_fee_bps: u16,
) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.authority)?;
    require!(mgmt_fee_bps <= MAX_CREATOR_FEE_BPS, VaultError::FeeTooHigh);
    check_policy(rebalance_mode, interval_secs, drift_bps)?;

    let basket = &mut ctx.accounts.basket;
    if basket.mgmt_fee_bps != mgmt_fee_bps {
        basket.last_fee_accrual = Clock::get()?.unix_timestamp;
    }
    basket.rebalance_mode = rebalance_mode;
    basket.interval_secs = interval_secs;
    basket.drift_bps = drift_bps;
    basket.mgmt_fee_bps = mgmt_fee_bps;
    Ok(())
}

/// Adds a wallet to an allowlisted basket. Creator-only.
pub fn allow_wallet(ctx: Context<AllowWallet>) -> Result<()> {
    let entry = &mut ctx.accounts.allow_entry;
    entry.bump = ctx.bumps.allow_entry;
    entry.basket = ctx.accounts.basket.key();
    entry.wallet = ctx.accounts.wallet.key();
    Ok(())
}

/// Holder governance was removed. A basket that had been handed to its holders is
/// otherwise frozen (no governor can act for it any more), so the protocol authority can
/// return it to its creator. Only ever acts on such a basket, and none can be created now.
pub fn restore_creator_control(ctx: Context<RestoreControl>) -> Result<()> {
    let basket = &mut ctx.accounts.basket;
    require!(basket.control == ControlMode::Dao, VaultError::NotDaoBasket);
    basket.control = ControlMode::Creator;
    // These bytes held its governor's address; clear them for their new use.
    basket.rebalance_supply = 0;
    basket.reserved = [0; 24];
    Ok(())
}

/// Splits (or reverse-splits) the basket's shares by `numerator`:`denominator` by
/// changing the share mint's display multiplier. Balances shown in wallets scale by the
/// ratio and NAV per share by its inverse; no tokens move and nothing needs minting.
/// Only the creator of a creator-managed basket can split it.
pub fn split(ctx: Context<Split>, numerator: u16, denominator: u16) -> Result<()> {
    authorise_mandate(&ctx.accounts.basket, &ctx.accounts.authority)?;
    require!(
        (1..=100).contains(&numerator) && (1..=100).contains(&denominator) && numerator != denominator,
        VaultError::BadParameter
    );
    let now = Clock::get()?.unix_timestamp;
    let current = {
        let data = ctx.accounts.share_mint.try_borrow_data()?;
        let last = extensions::last_split_at(&data).ok_or(VaultError::SplitUnsupported)?;
        require!(last == 0 || now >= last + SPLIT_COOLDOWN_SECS, VaultError::SplitTooSoon);
        extensions::current_multiplier(&data, now).ok_or(VaultError::SplitUnsupported)?
    };
    let next = current * numerator as f64 / denominator as f64;
    require!(next.is_finite() && (1e-6..=1e6).contains(&next), VaultError::BadParameter);

    let basket = &ctx.accounts.basket;
    let creator = basket.creator;
    let basket_id = basket.basket_id;
    let bump = [basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    invoke_signed(
        &extensions::update_multiplier(&ctx.accounts.token_program.key(), &ctx.accounts.share_mint.key(), &basket.key(), next, now),
        &[ctx.accounts.share_mint.to_account_info(), ctx.accounts.basket.to_account_info()],
        &[&seeds],
    )?;
    Ok(())
}

pub(crate) fn create_share_mint(ctx: &Context<InitializeBasket>) -> Result<()> {
    let token_program = ctx.accounts.share_token_program.key();
    let t22 = token_program == anchor_spl::token_2022::ID;
    require!(t22 || token_program == anchor_spl::token::ID, VaultError::BadParameter);
    let space = if t22 { extensions::MINT_WITH_SCALED_UI_LEN } else { extensions::PLAIN_MINT_LEN };
    let basket_key = ctx.accounts.basket.key();
    let bump = [ctx.bumps.share_mint];
    let seeds: [&[u8]; 3] = [b"share", basket_key.as_ref(), &bump];
    let system = ctx.accounts.system_program.to_account_info();
    let creator = ctx.accounts.creator.to_account_info();
    let mint = ctx.accounts.share_mint.to_account_info();
    let rent = Rent::get()?.minimum_balance(space);
    // The share mint's address is known before the basket exists (the app puts it in the
    // basket's lookup table first), and anyone can send lamports to an address. `create_account`
    // refuses one that holds any, so a few lamports sent there first would block the basket for
    // good. Such an account is still the system program's and empty, so it's topped up to rent,
    // given its space and handed to the token program instead.
    if mint.lamports() == 0 {
        system_program::create_account(
            CpiContext::new_with_signer(system.clone(), system_program::CreateAccount { from: creator, to: mint.clone() }, &[&seeds]),
            rent,
            space as u64,
            &token_program,
        )?;
    } else {
        let short = rent.saturating_sub(mint.lamports());
        if short > 0 {
            system_program::transfer(CpiContext::new(system.clone(), system_program::Transfer { from: creator, to: mint.clone() }), short)?;
        }
        system_program::allocate(CpiContext::new_with_signer(system.clone(), system_program::Allocate { account_to_allocate: mint.clone() }, &[&seeds]), space as u64)?;
        system_program::assign(CpiContext::new_with_signer(system, system_program::Assign { account_to_assign: mint.clone() }, &[&seeds]), &token_program)?;
    }
    if t22 {
        // Multiplier 1 to start; the basket PDA is the only authority that can change it.
        invoke(&extensions::initialize_scaled_ui(&token_program, mint.key, &basket_key, 1.0), &[mint.clone()])?;
    }
    invoke(
        &spl_token_2022::instruction::initialize_mint2(&token_program, mint.key, &basket_key, None, SHARE_DECIMALS)?,
        &[mint],
    )?;
    Ok(())
}

/// A schedule (unless drift-only) of at least a day, and a drift tolerance (unless
/// schedule-only) of at least 0.5%.
pub(crate) fn check_policy(mode: RebalanceMode, interval_secs: i64, drift_bps: u16) -> Result<()> {
    if mode != RebalanceMode::Drift {
        require!(interval_secs >= MIN_POLICY_INTERVAL_SECS, VaultError::BadPolicy);
    }
    if mode != RebalanceMode::Periodic {
        require!(drift_bps >= MIN_POLICY_DRIFT_BPS && (drift_bps as u128) <= BPS_DENOM, VaultError::BadPolicy);
    }
    Ok(())
}

pub(crate) fn authorise_mandate(basket: &Account<Basket>, authority: &Signer) -> Result<()> {
    match basket.control {
        ControlMode::Immutable => Err(VaultError::BasketImmutable.into()),
        ControlMode::Creator => {
            require_keys_eq!(authority.key(), basket.creator, VaultError::NotMandateAuthority);
            Ok(())
        }
        ControlMode::Dao => Err(VaultError::GovernanceRemoved.into()),
    }
}

// ---------------------------------------------------------------------------- accounts

#[derive(Accounts)]
#[instruction(args: InitBasketArgs)]
pub struct InitializeBasket<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    #[account(
        init,
        payer = creator,
        space = 8 + Basket::INIT_SPACE,
        seeds = [b"basket", creator.key().as_ref(), args.basket_id.as_ref()],
        bump
    )]
    pub basket: Account<'info, Basket>,

    /// CHECK: created and initialised in the instruction (see `create_share_mint`): a
    /// Token-2022 mint with a scaled UI amount, so the basket can split its shares later,
    /// or a classic SPL mint when the classic token program is passed.
    #[account(mut, seeds = [b"share", basket.key().as_ref()], bump)]
    pub share_mint: UncheckedAccount<'info>,

    pub share_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Reconfigure<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
}

#[derive(Accounts)]
pub struct AddConstituent<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
    /// CHECK: the asset's mint, matched against its listing entry.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: the asset's listing entry, read by `load_asset_entry`.
    pub asset_entry: UncheckedAccount<'info>,
    /// CHECK: the basket's vault for the asset, checked by `read_vault` (canonical ATA).
    pub vault: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct AllowWallet<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    #[account(has_one = creator)]
    pub basket: Account<'info, Basket>,

    /// CHECK: recorded as the permitted wallet; never signed or written through.
    pub wallet: UncheckedAccount<'info>,

    #[account(
        init,
        payer = creator,
        space = 8 + AllowEntry::INIT_SPACE,
        seeds = [b"allow", basket.key().as_ref(), wallet.key().as_ref()],
        bump
    )]
    pub allow_entry: Account<'info, AllowEntry>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RestoreControl<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    #[account(mut)]
    pub basket: Account<'info, Basket>,
}

#[derive(Accounts)]
pub struct Split<'info> {
    pub authority: Signer<'info>,
    #[account(has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    /// CHECK: the basket's share mint (has_one above); Token-2022 validates the extension.
    #[account(mut)]
    pub share_mint: UncheckedAccount<'info>,
    #[account(address = anchor_spl::token_2022::ID)]
    /// CHECK: the Token-2022 program.
    pub token_program: UncheckedAccount<'info>,
}
