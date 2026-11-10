//! Minting and redeeming: units of every token in, shares out, and back. No price is involved in
//! what moves; the protocol's fee, paid in USDC where it can be (`usd_fee`), is priced from the
//! posted prices a rebalance uses.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::instructions::invite::invited;
use crate::params::{self, Params, PARAMS_ID};
use crate::state::*;
use crate::token_util::*;
use anchor_lang::solana_program::sysvar::instructions::{load_current_index_checked, load_instruction_at_checked};

/// First deposit. Fixes `units_per_share` for every sleeve and mints the opening shares.
/// Callable once, by the creator, regardless of access mode.
pub fn seed_basket<'info>(
    ctx: Context<'_, '_, 'info, 'info, SeedBasket<'info>>,
    shares: u64,
    units_per_share: Vec<u64>,
) -> Result<()> {
    require!(!ctx.accounts.basket.seeded, VaultError::AlreadySeeded);
    require!(!ctx.accounts.basket.mint_paused, VaultError::MintPaused);
    let prm = Params::load(&ctx.accounts.params)?;
    prm.require_on(params::FAMILY_MINT)?;
    require!(shares > 0, VaultError::ZeroShares);
    require_keys_eq!(
        ctx.accounts.payer.key(),
        ctx.accounts.basket.manager(),
        VaultError::MintNotPermitted
    );
    require!(
        units_per_share.len() == ctx.accounts.basket.constituent_count as usize,
        VaultError::BadRemainingAccounts
    );
    // The quads, then the fee accounts if the creator pays the fee in USDC, and nothing else.
    let (quads, extra) = split_quads(ctx.remaining_accounts, ctx.accounts.basket.constituent_count)?;
    let (fees, extra) = split_fee_accounts(extra)?;
    require!(extra.is_empty(), VaultError::BadRemainingAccounts);
    require!(units_per_share.iter().all(|u| *u > 0), VaultError::ZeroShares);

    for (i, u) in units_per_share.iter().enumerate() {
        ctx.accounts.basket.constituents[i].units_per_share = *u;
    }
    ctx.accounts.basket.seeded = true;
    // The creator fee is owed from when shares exist, not from when the basket was created:
    // a basket left unseeded for months would otherwise charge its first holders for them.
    ctx.accounts.basket.last_fee_accrual = Clock::get()?.unix_timestamp;

    // The opening deposit pays the fee like any other: fee-free, a basket of one's own
    // would be the way to buy in without it.
    let a = &mut *ctx.accounts;
    let deposit = Deposit {
        payer: &a.payer,
        basket: &mut a.basket,
        share_mint: &a.share_mint,
        payer_shares: &a.payer_shares,
        protocol: &a.protocol,
        burn_shares: &a.burn_shares,
        burn_authority: a.burn_authority.key(),
        treasury_shares: &a.treasury_shares,
        share_token_program: &a.share_token_program,
        invited: false,
        creator_cut_bps: 0,
        min_net: 0,
        fee_cap: prm.bps(params::TRANSFER_FEE_CAP_BPS),
        fees,
        flow: FLOW_SEED,
        cap: shares,
    };
    collect_and_mint(deposit, quads, shares)
}

/// Deposit the exact underlying basket for up to `shares`, receive them less the
/// protocol's mint fee. No oracle is consulted.
///
/// With `min_shares` below `shares`, mints as many as the payer's token balances cover,
/// but never fewer than `min_shares`. That is how a wallet buys with swaps whose output
/// isn't known to the unit when it signs: it pre-signs this with the quoted amount and
/// its slippage floor, and nothing is left over except what legs filled unevenly.
///
/// With `MIN_NET_FLAG` set in `min_shares`, the rest of it is a floor on the shares the payer
/// receives, after the protocol's fee and the creator's cut, so a fee raised between signing and
/// landing can't take more than the buyer agreed to. Without it, it floors the shares minted
/// before them, as it always did.
///
/// A referred wallet passes its invite after the quads and pays half the fee (`Invite`).
pub fn mint_in_kind<'info>(
    ctx: Context<'_, '_, 'info, 'info, MintInKind<'info>>,
    shares: u64,
    min_shares: u64,
) -> Result<()> {
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
    let prm = Params::load(&ctx.accounts.params)?;
    prm.require_on(params::FAMILY_MINT)?;
    let (quads, extra) = split_quads(ctx.remaining_accounts, ctx.accounts.basket.constituent_count)?;
    let (fees, extra) = split_fee_accounts(extra)?;
    let invited = invited(extra, &ctx.accounts.payer.key())?;
    require!(!ctx.accounts.basket.mint_paused, VaultError::MintPaused);
    // Value is on its way between chains (`trip.rs`): a mint now would buy without it.
    require!(!ctx.accounts.basket.flag(TRIP_OPEN), VaultError::TripOpen);
    let net_floor = min_shares & MIN_NET_FLAG != 0;
    let floor = min_shares & !MIN_NET_FLAG;
    require!(floor <= shares, VaultError::BadParameter);
    let cap = shares;
    let shares = if floor < shares { affordable_shares(&ctx, shares)? } else { shares };
    require!(shares > 0, VaultError::ZeroShares);
    require!(net_floor || shares >= floor, VaultError::BelowMinShares);

    let basket = &ctx.accounts.basket;
    let payer = ctx.accounts.payer.key();
    // With no shares out, a mint isn't buying a slice of anything: it would own the vaults
    // outright, whatever was left behind in them (a forfeited sleeve, tokens sent there). Only
    // the creator can open an emptied basket again, as they opened it with the seed.
    if ctx.accounts.share_mint.supply == 0 {
        require_keys_eq!(payer, basket.manager(), VaultError::BasketEmpty);
    }
    match basket.access {
        AccessMode::Public => {}
        AccessMode::Private => {
            require_keys_eq!(payer, basket.manager(), VaultError::MintNotPermitted);
        }
        AccessMode::Allowlist => {
            let entry = ctx
                .accounts
                .allow_entry
                .as_ref()
                .ok_or(VaultError::MintNotPermitted)?;
            require_keys_eq!(entry.basket, basket.key(), VaultError::MintNotPermitted);
            require_keys_eq!(entry.wallet, payer, VaultError::MintNotPermitted);
        }
    }

    // The manager's own mints would only pay themselves; on a vote-run basket the cut is its share
    // stakers', so the manager pays it like anyone.
    let creator_cut_bps = if payer == basket.manager() && !basket.by_vote() { 0 } else { basket.cut_bps() };

    let a = &mut *ctx.accounts;
    let deposit = Deposit {
        payer: &a.payer,
        basket: &mut a.basket,
        share_mint: &a.share_mint,
        payer_shares: &a.payer_shares,
        protocol: &a.protocol,
        burn_shares: &a.burn_shares,
        burn_authority: a.burn_authority.key(),
        treasury_shares: &a.treasury_shares,
        share_token_program: &a.share_token_program,
        invited,
        creator_cut_bps,
        min_net: if net_floor { floor } else { 0 },
        fee_cap: prm.bps(params::TRANSFER_FEE_CAP_BPS),
        fees,
        flow: FLOW_MINT,
        cap,
    };
    collect_and_mint(deposit, quads, shares)
}

/// Hand in `shares`: the rest are burned and their underlying paid out pro-rata, less the
/// protocol's redeem fee. Never gated — not by access mode, not by a delisting, not by the fee.
/// A holder who cannot redeem holds a promise, not a claim.
///
/// The fee is paid in USDC from the redeemer's wallet when the fee accounts follow the quads and
/// it can be (`usd_fee`); otherwise, as before, it's kept in shares. A wallet with no USDC, no
/// USDC account or a token with no fresh price still redeems: a missing price never blocks an exit.
///
/// `forfeit_mask` (bit i = sleeve i, normally 0) is the way out when an issuer has
/// paused or frozen one token: the redeemer leaves those sleeves behind and takes the
/// rest. What they leave stays in the vault for the remaining holders: the units are
/// re-derived from the vaults at the end, so it's theirs at once.
///
/// Payouts are priced from the vaults' balances (`Basket::live_units`), not from stored
/// units: a surplus is shared with the redeemer, a clawback's deficit is borne pro rata.
///
/// Remaining accounts: the quads, the fee accounts (optional), an invite (optional: a referred
/// wallet pays half the fee), and while a trip is out the trip's three accounts.
pub fn redeem_in_kind<'info>(
    ctx: Context<'_, '_, 'info, 'info, RedeemInKind<'info>>,
    shares: u64,
    forfeit_mask: u16,
) -> Result<()> {
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
    require!(shares > 0, VaultError::ZeroShares);
    // Before anything is burned: what each vault holds is shared over this supply.
    let supply_before = ctx.accounts.share_mint.supply;

    let count = ctx.accounts.basket.constituent_count as usize;
    require!((forfeit_mask as u32) < (1u32 << count), VaultError::BadParameter);
    let (quads, extra) = split_quads(ctx.remaining_accounts, ctx.accounts.basket.constituent_count)?;
    // While a trip is out, its last three extra accounts are the trip, the redeemer's IOU and the
    // system program: the redeemer's slice of the trip is kept for them (`trip.rs`).
    let trip_open = ctx.accounts.basket.flag(TRIP_OPEN);
    let (extra, trip_accounts) = if trip_open {
        require!(extra.len() >= 3, VaultError::BadRemainingAccounts);
        extra.split_at(extra.len() - 3)
    } else {
        (extra, &extra[..0])
    };
    let (fees, extra) = split_fee_accounts(extra)?;
    let invited = invited(extra, &ctx.accounts.payer.key())?;

    let basket_key = ctx.accounts.basket.key();
    // Every quad checked once, and what each vault holds read once, before anything moves.
    let mut held = [0u64; MAX_CONSTITUENTS];
    for i in 0..count {
        if forfeit_mask & (1 << i) != 0 {
            continue;
        }
        let c = ctx.accounts.basket.constituents[i];
        let mint_ai = &quads[i * 4 + 2];
        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        // The mint's owner *is* its token program; anything else would let a caller
        // route the transfer through a program of their choosing.
        require_keys_eq!(*quads[i * 4 + 3].key, *mint_ai.owner, VaultError::ConstituentMismatch);
        held[i] = read_vault(&quads[i * 4], &basket_key, &c.mint)?.amount;
    }

    let usd = usd_fee(fees.as_ref(), &ctx.accounts.basket, &basket_key, &ctx.accounts.protocol, &ctx.accounts.burn_authority.key(), &ctx.accounts.payer.key(), shares, shares, invited, 0)?;
    let shares = match usd {
        Some(fee) => {
            pay_usd_fee(fees.as_ref().unwrap(), &ctx.accounts.payer.to_account_info(), &fee)?;
            emit!(UsdFeePaid {
                basket: basket_key,
                payer: ctx.accounts.payer.key(),
                flow: FLOW_REDEEM,
                shares,
                value: fee.value,
                usdc: fee.protocol(),
                burn: fee.burn,
                treasury: fee.treasury,
                stakers: fee.stakers,
                cut: 0,
                cut_to_stakers: false,
            });
            shares
        }
        None => {
            let (fee, burn_fee, treasury_fee) = protocol_cut(shares, &ctx.accounts.protocol, &ctx.accounts.basket, invited)?;
            require!(shares > fee, VaultError::BelowFee);
            owe_stakers(&mut ctx.accounts.basket, &ctx.accounts.protocol, fee, treasury_fee)?;
            for (to, amount) in [
                (ctx.accounts.burn_shares.to_account_info(), burn_fee),
                (ctx.accounts.treasury_shares.to_account_info(), treasury_fee),
            ] {
                if amount > 0 {
                    token_interface::transfer_checked(
                        CpiContext::new(
                            ctx.accounts.share_token_program.to_account_info(),
                            token_interface::TransferChecked {
                                from: ctx.accounts.payer_shares.to_account_info(),
                                mint: ctx.accounts.share_mint.to_account_info(),
                                to,
                                authority: ctx.accounts.payer.to_account_info(),
                            },
                        ),
                        amount,
                        ctx.accounts.share_mint.decimals,
                    )?;
                }
            }
            shares - fee
        }
    };

    // Burn first: a reentrant token program cannot be handed a vault that still
    // counts the shares it is about to destroy.
    token_interface::burn(
        CpiContext::new(
            ctx.accounts.share_token_program.to_account_info(),
            token_interface::Burn {
                mint: ctx.accounts.share_mint.to_account_info(),
                from: ctx.accounts.payer_shares.to_account_info(),
                authority: ctx.accounts.payer.to_account_info(),
            },
        ),
        shares,
    )?;

    let creator = ctx.accounts.basket.creator;
    let basket_id = ctx.accounts.basket.basket_id;
    let bump = [ctx.accounts.basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let signer: &[&[&[u8]]] = &[&seeds];

    for i in 0..count {
        if forfeit_mask & (1 << i) != 0 {
            continue;
        }
        let vault_ata = &quads[i * 4];
        let user_ata = &quads[i * 4 + 1];
        let mint_ai = &quads[i * 4 + 2];
        let token_program = &quads[i * 4 + 3];

        // Read again, not assumed: the vault checked above, whatever the fee's transfers did.
        let vault_amount = read_token_account(vault_ata)?.amount;
        require!(vault_amount == held[i], VaultError::VaultUndercollateralised);
        let amount = Basket::slice_of(vault_amount, shares, supply_before, false)?;
        // Can't happen with a slice of the balance, rounded down; kept as the
        // last line between a mistake and paying early redeemers out of later ones.
        require!(
            vault_amount >= amount,
            VaultError::VaultUndercollateralised
        );

        let decimals = mint_decimals(mint_ai)?;
        transfer_from_vault(
            mint_ai,
            vault_ata,
            user_ata,
            &ctx.accounts.basket.to_account_info(),
            token_program,
            amount,
            decimals,
            signer,
        )?;
    }

    if trip_open {
        crate::instructions::trip::record_iou(&basket_key, &trip_accounts[0], &trip_accounts[1], &trip_accounts[2], &ctx.accounts.payer.to_account_info(), shares)?;
    }

    // What's left, forfeited sleeves included, backs the shares still out.
    resync_units(&mut ctx.accounts.basket, quads, 4, &basket_key, supply_before - shares)
}

/// Re-derives every sleeve's units from its vault and `supply` (vaults at `stride` steps in
/// `remaining`): after a redemption, so what stays behind is the remaining holders' at once.
pub(crate) fn resync_units(basket: &mut Basket, remaining: &[AccountInfo], stride: usize, basket_key: &Pubkey, supply: u64) -> Result<()> {
    if supply == 0 {
        return Ok(());
    }
    for i in 0..basket.constituent_count as usize {
        let mint = basket.constituents[i].mint;
        let held = read_vault(&remaining[i * stride], basket_key, &mint)?.amount;
        basket.constituents[i].units_per_share = Basket::live_units(held, supply, basket.constituents[i].units_per_share)?;
    }
    Ok(())
}

/// The accounts a deposit moves tokens and shares through, from `SeedBasket` or `MintInKind`.
pub(crate) struct Deposit<'a, 'info> {
    pub payer: &'a Signer<'info>,
    pub basket: &'a mut Account<'info, Basket>,
    pub share_mint: &'a InterfaceAccount<'info, MintI>,
    pub payer_shares: &'a InterfaceAccount<'info, TokenAccountI>,
    pub protocol: &'a Account<'info, Protocol>,
    pub burn_shares: &'a InterfaceAccount<'info, TokenAccountI>,
    pub burn_authority: Pubkey,
    pub treasury_shares: &'a InterfaceAccount<'info, TokenAccountI>,
    pub share_token_program: &'a Interface<'info, TokenInterface>,
    /// The payer trades on a live invite: half the fee, none of it burned.
    pub invited: bool,
    /// The creator's cut of this deposit. An invite doesn't change it.
    pub creator_cut_bps: u16,
    /// The fewest shares the payer must receive, after the fee and the cut.
    pub min_net: u64,
    /// The highest transfer fee an asset may charge to take new money (`TRANSFER_FEE_CAP_BPS`).
    pub fee_cap: u16,
    /// The fee accounts, if the payer passed them (`usd_fee`).
    pub fees: Option<FeeAccounts<'a, 'info>>,
    /// `FLOW_SEED` or `FLOW_MINT`, for the event.
    pub flow: u8,
    /// The shares the price key's value is for: the most the payer asked for.
    pub cap: u64,
}

/// Shared body of `seed_basket` and `mint_in_kind`: pull every sleeve in, take the fee, then mint.
/// `remaining` is one `[vault_ata, user_ata, mint, token_program]` quad per constituent.
pub(crate) fn collect_and_mint<'info>(d: Deposit<'_, 'info>, remaining: &[AccountInfo<'info>], shares: u64) -> Result<()> {
    let count = d.basket.constituent_count as usize;
    require!(
        remaining.len() == count * 4,
        VaultError::BadRemainingAccounts
    );

    let basket_key = d.basket.key();
    let payer_key = d.payer.key();
    // Before anything is minted: the deposit buys a slice of what the vaults hold now.
    let supply = d.share_mint.supply;

    for i in 0..count {
        let c = d.basket.constituents[i];
        let vault_ata = &remaining[i * 4];
        let user_ata = &remaining[i * 4 + 1];
        let mint_ai = &remaining[i * 4 + 2];
        let token_program = &remaining[i * 4 + 3];

        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(*token_program.key, *mint_ai.owner, VaultError::ConstituentMismatch);

        let vault_before = read_vault(vault_ata, &basket_key, &c.mint)?.amount;

        let user_view = read_token_account(user_ata)?;
        require_keys_eq!(user_view.mint, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(user_view.owner, payer_key, VaultError::ConstituentMismatch);

        // Priced from the vault, not stored units: tokens that arrived outside the program
        // already belong to the holders, and the deposit pays for its slice of them.
        d.basket.constituents[i].units_per_share = Basket::live_units(vault_before, supply, c.units_per_share)?;
        // A seed (no shares out yet) pays the units it set.
        let amount = if supply == 0 {
            Basket::amount_for(c.units_per_share, shares, true)?
        } else {
            Basket::slice_of(vault_before, shares, supply, true)?
        };
        let decimals = mint_decimals(mint_ai)?;
        // A Token-2022 transfer fee is the payer's to bear: they send enough that the vault
        // is credited the full amount. New money stops if the fee is raised past the cap.
        let fee = TransferFee::of(mint_ai)?;
        if amount > 0 {
            fee.assert_within_cap(d.fee_cap)?;
        }
        transfer_from_user(
            mint_ai,
            user_ata,
            vault_ata,
            &d.payer.to_account_info(),
            token_program,
            fee.gross(amount)?,
            decimals,
        )?;
        // Measured, not assumed: whatever the fee did, the vault must hold the full amount.
        let received = read_token_account(vault_ata)?.amount.saturating_sub(vault_before);
        require!(received >= amount, VaultError::DepositShort);
    }

    let creator = d.basket.creator;
    let basket_id = d.basket.basket_id;
    let bump = [d.basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let signer: &[&[&[u8]]] = &[&seeds];

    // The protocol's fee: in USDC from the payer's wallet where it can be, else (as before) a cut
    // of `shares` minted to the burn PDA and the treasury, so units and collateral are untouched
    // either way. The creator's cut likewise: in USDC to the manager (or a vote-run basket's share
    // stakers' pot), else kept in the treasury's shares and counted on the basket.
    let usd = usd_fee(d.fees.as_ref(), d.basket, &basket_key, d.protocol, &d.burn_authority, &payer_key, shares, d.cap, d.invited, d.creator_cut_bps)?;
    let (fee, burn_fee, treasury_fee) = match &usd {
        Some(paid) => {
            pay_usd_fee(d.fees.as_ref().unwrap(), &d.payer.to_account_info(), paid)?;
            emit!(UsdFeePaid {
                basket: basket_key,
                payer: payer_key,
                flow: d.flow,
                shares,
                value: paid.value,
                usdc: paid.protocol(),
                burn: paid.burn,
                treasury: paid.treasury,
                stakers: paid.stakers,
                cut: paid.cut,
                cut_to_stakers: paid.cut_to_stakers,
            });
            (0, 0, 0)
        }
        None => protocol_cut(shares, d.protocol, d.basket, d.invited)?,
    };
    // The creator's cut in shares, unless it was paid in USDC.
    let cut = if usd.as_ref().is_some_and(|u| u.cut > 0) { 0 } else { flow_fee(shares, d.creator_cut_bps)? };
    let kept = fee.checked_add(cut).ok_or(VaultError::MathOverflow)?;
    require!(shares > kept, VaultError::BelowFee);
    require!(shares - kept >= d.min_net, VaultError::BelowMinShares);
    d.basket.creator_unclaimed = d.basket.creator_unclaimed.checked_add(cut).ok_or(VaultError::MathOverflow)?;
    if usd.is_none() {
        owe_stakers(d.basket, d.protocol, fee, treasury_fee)?;
    }
    let program = d.share_token_program.to_account_info();
    let mint = d.share_mint.to_account_info();
    let authority = d.basket.to_account_info();
    for (to, amount) in [
        (d.payer_shares.to_account_info(), shares - kept),
        (d.burn_shares.to_account_info(), burn_fee),
        (d.treasury_shares.to_account_info(), treasury_fee + cut),
    ] {
        if amount > 0 {
            mint_shares(&program, &mint, &to, &authority, signer, amount)?;
        }
    }
    Ok(())
}

/// The most shares (up to `cap`) the payer's balances cover. A deposit for `s` shares
/// takes `ceil(units * s / ONE_SHARE)` of a sleeve, which a balance `b` covers exactly
/// when `s <= b * ONE_SHARE / units`. Remaining accounts are the mint quads.
pub(crate) fn affordable_shares<'info>(ctx: &Context<'_, '_, 'info, 'info, MintInKind<'info>>, cap: u64) -> Result<u64> {
    let count = ctx.accounts.basket.constituent_count as usize;
    let (quads, _) = split_quads(ctx.remaining_accounts, ctx.accounts.basket.constituent_count)?;
    let payer = ctx.accounts.payer.key();
    let basket_key = ctx.accounts.basket.key();
    let supply = ctx.accounts.share_mint.supply;
    let mut shares = cap as u128;
    for i in 0..count {
        let c = ctx.accounts.basket.constituents[i];
        // The same slice of the vault `collect_and_mint` will charge.
        let held = read_vault(&quads[i * 4], &basket_key, &c.mint)?.amount;
        let units = Basket::live_units(held, supply, c.units_per_share)?;
        if units == 0 {
            continue;
        }
        let user = read_token_account(&quads[i * 4 + 1])?;
        require_keys_eq!(user.mint, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(user.owner, payer, VaultError::ConstituentMismatch);
        let mint_ai = &quads[i * 4 + 2];
        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        let usable = TransferFee::of(mint_ai)?.net(user.amount)?;
        // The most shares whose deposit fits in `usable`: through units while they fit (as
        // `slice_of` prices), straight from the vault past them.
        let most = if units < u64::MAX { usable as u128 * ONE_SHARE / units as u128 } else { usable as u128 * supply as u128 / held as u128 };
        shares = shares.min(most);
    }
    Ok(shares as u64)
}

/// The protocol's cut of a seed, mint or redeem, rounded up. Rounded down, any flow under
/// 10,000 / fee_bps raw units paid nothing, and a basket seeded with few raw units for a lot
/// of tokens made such a flow worth thousands: minted and redeemed in small enough pieces,
/// it never paid the fee at all.
pub(crate) fn flow_fee(shares: u64, fee_bps: u16) -> Result<u64> {
    let fee = ((shares as u128) * (fee_bps as u128)).div_ceil(BPS_DENOM);
    u64::try_from(fee).map_err(|_| VaultError::MathOverflow.into())
}

/// The protocol's cut of `shares` and how it splits: `(fee, burn, treasury)`. A smart basket
/// pays the Smart-mode fee (`smart_fee_bps`), split the same way. An invited payer pays half
/// the rate, rounded up, all of it to the treasury.
pub(crate) fn protocol_cut(shares: u64, protocol: &Account<Protocol>, basket: &Basket, invited: bool) -> Result<(u64, u64, u64)> {
    let (fee_bps, burn_share_bps) = protocol_fees(protocol);
    let rate = if basket.control == ControlMode::Smart { smart_fee_bps(&protocol.to_account_info()) } else { fee_bps };
    if invited {
        let fee = flow_fee(shares, rate.div_ceil(2))?;
        return Ok((fee, 0, fee));
    }
    let fee = flow_fee(shares, rate)?;
    let (burn, treasury) = split_protocol_fee(fee, burn_share_bps)?;
    Ok((fee, burn, treasury))
}

/// Counts the stakers' share of `fee` on the basket once staking is on: it stays in the treasury's
/// shares, which `stake_sweep` moves it out of and `treasury_withdraw` leaves alone. Rounded up,
/// never past the treasury's cut.
fn owe_stakers(basket: &mut Basket, protocol: &Account<Protocol>, fee: u64, treasury_fee: u64) -> Result<()> {
    if protocol_flags(&protocol.to_account_info()) & FLAG_STAKING == 0 {
        return Ok(());
    }
    let share = u64::try_from((fee as u128 * STAKER_SHARE_BPS).div_ceil(BPS_DENOM)).map_err(|_| VaultError::MathOverflow)?.min(treasury_fee);
    basket.stake_owed = basket.stake_owed.checked_add(share).ok_or(VaultError::MathOverflow)?;
    Ok(())
}

/// A mint's or redeem's remaining accounts: one quad per constituent, then what follows them
/// (nothing, or an invite).
pub(crate) fn split_quads<'a, 'info>(remaining: &'a [AccountInfo<'info>], count: u8) -> Result<(&'a [AccountInfo<'info>], &'a [AccountInfo<'info>])> {
    let n = count as usize * 4;
    require!(remaining.len() >= n, VaultError::BadRemainingAccounts);
    Ok(remaining.split_at(n))
}

/// Splits a protocol fee between the burn PDA (for buying and burning GRAVI) and the treasury.
/// The burn's cut rounds up (never past the fee), so it is at least `burn_share_bps` of every
/// fee, odd ones included: rounded down, an odd fee at the 50% floor burned less than half.
pub(crate) fn split_protocol_fee(fee: u64, burn_share_bps: u16) -> Result<(u64, u64)> {
    let burn = u64::try_from(((fee as u128) * (burn_share_bps as u128)).div_ceil(BPS_DENOM))
        .map_err(|_| VaultError::MathOverflow)?
        .min(fee);
    Ok((burn, fee - burn))
}

// ------------------------------------------------------------------------- the fee in USDC

/// The mint fees are paid in: USDC.
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
/// The fee accounts: `[instructions_sysvar, payer_usdc, burn_usdc, treasury_usdc, rewards_usdc,
/// cut_usdc, token_program]`.
pub const FEE_ACCOUNTS: usize = 7;
/// The owner of a vote-run basket's USDC pot for its share stakers: `[CUT_POT_SEED, basket]`.
pub const CUT_POT_SEED: &[u8] = b"cut_pot";

pub const FLOW_SEED: u8 = 0;
pub const FLOW_MINT: u8 = 1;
pub const FLOW_REDEEM: u8 = 2;

/// Fees paid in USDC (base units, six decimals). Logged so the app's history, P&L and referral
/// credit read them from the transaction, not from share movements.
#[event]
pub struct UsdFeePaid {
    pub basket: Pubkey,
    pub payer: Pubkey,
    /// `FLOW_SEED`, `FLOW_MINT` or `FLOW_REDEEM`.
    pub flow: u8,
    /// The shares minted or redeemed.
    pub shares: u64,
    /// The trade's value the price key signed, scaled to these shares.
    pub value: u64,
    /// The protocol's fee, and how it split.
    pub usdc: u64,
    pub burn: u64,
    pub treasury: u64,
    pub stakers: u64,
    /// The creator's cut, paid to the manager, or to the share stakers' pot of a vote-run basket.
    pub cut: u64,
    pub cut_to_stakers: bool,
}

/// The price key vouches for the dollar value of the flow right after it in this transaction, in
/// USDC base units: what the tokens a mint deposits (for its most shares) or a redeem pays out are
/// worth at its prices. The flow takes the fees from it at the rates the program holds. Moves
/// nothing. The signature covers the whole transaction, and a transaction only lands within its
/// blockhash's minute or two, so a value can't be kept for later or moved to another trade.
pub fn fee_quote(_ctx: Context<SignFeeQuote>, value: u64) -> Result<()> {
    require!(value > 0, VaultError::BadParameter);
    Ok(())
}

/// What a flow names to pay its fees in USDC: right after its quads, first of them the instructions
/// sysvar (which is how they're told from an invite, the only other thing that may follow), then
/// the payer's USDC account (any of theirs; it may not exist), the burn PDA's, the protocol's and the
/// stakers' rewards PDA's USDC accounts (their ATAs), where the creator's cut goes (a USDC account of
/// the manager's, or the share stakers' pot of a vote-run basket; anything on a flow without a cut),
/// and the SPL Token program.
pub(crate) struct FeeAccounts<'a, 'info> {
    pub instructions: &'a AccountInfo<'info>,
    pub payer_usdc: &'a AccountInfo<'info>,
    pub burn_usdc: &'a AccountInfo<'info>,
    pub treasury_usdc: &'a AccountInfo<'info>,
    pub rewards_usdc: &'a AccountInfo<'info>,
    pub cut_usdc: &'a AccountInfo<'info>,
    pub token_program: &'a AccountInfo<'info>,
}

/// Splits the fee accounts off the front of what follows a flow's quads, if they're there.
pub(crate) fn split_fee_accounts<'a, 'info>(extra: &'a [AccountInfo<'info>]) -> Result<(Option<FeeAccounts<'a, 'info>>, &'a [AccountInfo<'info>])> {
    match extra.first() {
        Some(ai) if *ai.key == anchor_lang::solana_program::sysvar::instructions::ID => {
            require!(extra.len() >= FEE_ACCOUNTS, VaultError::BadRemainingAccounts);
            let (b, rest) = extra.split_at(FEE_ACCOUNTS);
            Ok((
                Some(FeeAccounts { instructions: &b[0], payer_usdc: &b[1], burn_usdc: &b[2], treasury_usdc: &b[3], rewards_usdc: &b[4], cut_usdc: &b[5], token_program: &b[6] }),
                rest,
            ))
        }
        _ => Ok((None, extra)),
    }
}

/// Fees paid in USDC and where they go, base units.
pub(crate) struct UsdFee {
    pub value: u64,
    pub burn: u64,
    pub treasury: u64,
    pub stakers: u64,
    /// The creator's cut in USDC; 0 when it's kept in shares (none due, or it can't be paid).
    pub cut: u64,
    pub cut_to_stakers: bool,
}

impl UsdFee {
    pub fn protocol(&self) -> u64 {
        self.burn + self.treasury + self.stakers
    }
}

/// The value the price key signed for this flow: a `fee_quote` right before it.
fn signed_value(instructions: &AccountInfo) -> Option<u64> {
    let at = load_current_index_checked(instructions).ok()? as usize;
    let ix = load_instruction_at_checked(at.checked_sub(1)?, instructions).ok()?;
    let disc = <crate::instruction::FeeQuote as anchor_lang::Discriminator>::DISCRIMINATOR;
    if ix.program_id != crate::ID || ix.data.len() != 16 || ix.data[..8] != *disc {
        return None;
    }
    // A `fee_quote` that's here ran: its signer is the price key, or the transaction failed.
    Some(u64::from_le_bytes(ix.data[8..16].try_into().ok()?))
}

fn bps_of(value: u64, bps: u16) -> Result<u64> {
    u64::try_from((value as u128 * bps as u128).div_ceil(BPS_DENOM)).map_err(|_| VaultError::MathOverflow.into())
}

/// The fees on a flow of `shares` in USDC, when they can be paid that way. `cap` is the shares the
/// signed value is for: a mint that came out smaller pays on its share of the value, rounded up.
/// - The protocol's fee: the value times the fee rate (the Smart rate on a smart basket, half of it,
///   rounded up, on an invite), rounded up to the next USDC unit. It splits as the shares did: the
///   burn's share to the burn PDA (rounded up), the stakers' 10% out of the treasury's side (once
///   staking is on), the rest to the treasury; an invite's all to the treasury's side.
/// - The creator's cut (`cut_bps`, mints only): the value times it, rounded up, to a USDC account
///   of the manager's, or for a vote-run basket to its share stakers' pot (`stake_cut.rs`).
///
/// Needs the fee accounts, the signed value, and the payer's USDC (not frozen) to cover the fee,
/// each side being its canonical account. Otherwise None, and the fee is taken in shares as it
/// always was: never an error, so a missing value, an empty wallet or a mistyped account never
/// stops a flow, and no flow skips the fee. The cut alone falls back to shares when its account
/// is wrong or the USDC covers only the fee.
#[allow(clippy::too_many_arguments)]
pub(crate) fn usd_fee(
    fees: Option<&FeeAccounts>,
    basket: &Basket,
    basket_key: &Pubkey,
    protocol: &Account<Protocol>,
    burn_authority: &Pubkey,
    payer: &Pubkey,
    shares: u64,
    cap: u64,
    invited: bool,
    cut_bps: u16,
) -> Result<Option<UsdFee>> {
    let Some(f) = fees else { return Ok(None) };
    if *f.token_program.key != anchor_spl::token::ID || shares == 0 || cap < shares {
        return Ok(None);
    }
    let Some(signed) = signed_value(f.instructions) else { return Ok(None) };
    let value = u64::try_from((signed as u128 * shares as u128).div_ceil(cap as u128)).map_err(|_| VaultError::MathOverflow)?;

    let (fee_bps, burn_share_bps) = protocol_fees(protocol);
    let rate = if basket.control == ControlMode::Smart { smart_fee_bps(&protocol.to_account_info()) } else { fee_bps };
    let fee = bps_of(value, if invited { rate.div_ceil(2) } else { rate })?;
    let (burn, treasury) = if invited { (0, fee) } else { split_protocol_fee(fee, burn_share_bps)? };
    let stakers = if protocol_flags(&protocol.to_account_info()) & FLAG_STAKING != 0 {
        u64::try_from((fee as u128 * STAKER_SHARE_BPS).div_ceil(BPS_DENOM)).map_err(|_| VaultError::MathOverflow)?.min(treasury)
    } else {
        0
    };

    let Some(held) = movable_balance(f.payer_usdc, &USDC_MINT, payer) else { return Ok(None) };
    if held < fee {
        return Ok(None);
    }
    let rewards = if stakers > 0 { Pubkey::find_program_address(&[STAKE_REWARDS_SEED], &crate::ID).0 } else { Pubkey::default() };
    for (ai, owner, amount) in [(f.burn_usdc, *burn_authority, burn), (f.treasury_usdc, protocol.key(), treasury - stakers), (f.rewards_usdc, rewards, stakers)] {
        if amount > 0 && (movable_balance(ai, &USDC_MINT, &owner).is_none() || !is_ata(ai, &owner, &USDC_MINT)) {
            return Ok(None);
        }
    }

    // The cut, where it can go in USDC too.
    let mut cut = if cut_bps > 0 { bps_of(value, cut_bps)? } else { 0 };
    let cut_to_stakers = basket.by_vote();
    if cut > 0 {
        let ok = held - fee >= cut
            && if cut_to_stakers {
                let pot = Pubkey::find_program_address(&[CUT_POT_SEED, basket_key.as_ref()], &crate::ID).0;
                movable_balance(f.cut_usdc, &USDC_MINT, &pot).is_some() && is_ata(f.cut_usdc, &pot, &USDC_MINT)
            } else {
                movable_balance(f.cut_usdc, &USDC_MINT, &basket.manager()).is_some()
            };
        if !ok {
            cut = 0;
        }
    }
    // Nothing to pay in USDC (a protocol fee of 0, and no cut it can take): as before.
    if fee == 0 && cut == 0 {
        return Ok(None);
    }
    Ok(Some(UsdFee { value, burn, treasury: treasury - stakers, stakers, cut, cut_to_stakers }))
}

/// Moves fees `usd_fee` settled from the payer's USDC account, the payer signing. A plain SPL
/// `Transfer` (USDC is a classic SPL token, and `usd_fee` read every end's mint), so no mint account
/// is needed: a big basket's flow is near the transaction's account limit.
pub(crate) fn pay_usd_fee<'info>(f: &FeeAccounts<'_, 'info>, payer: &AccountInfo<'info>, fee: &UsdFee) -> Result<()> {
    for (to, amount) in [(f.burn_usdc, fee.burn), (f.treasury_usdc, fee.treasury), (f.rewards_usdc, fee.stakers), (f.cut_usdc, fee.cut)] {
        if amount == 0 {
            continue;
        }
        anchor_spl::token::transfer(
            CpiContext::new(f.token_program.clone(), anchor_spl::token::Transfer { from: f.payer_usdc.clone(), to: to.clone(), authority: payer.clone() }),
            amount,
        )?;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct SignFeeQuote<'info> {
    pub price_authority: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = price_authority_of(&keepers.to_account_info()) == price_authority.key() @ VaultError::NotPriceAuthority)]
    pub keepers: Account<'info, Keepers>,
}

// ---------------------------------------------------------------------------- accounts

/// `MintInKind`'s accounts without the allowlist entry (the seed is the creator's), plus the
/// creation burn's.
#[derive(Accounts)]
pub struct SeedBasket<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(mut, has_one = share_mint)]
    pub basket: Box<Account<'info, Basket>>,

    #[account(mut)]
    pub share_mint: Box<InterfaceAccount<'info, MintI>>,

    #[account(mut, token::mint = share_mint, token::authority = payer)]
    pub payer_shares: Box<InterfaceAccount<'info, TokenAccountI>>,

    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Box<Account<'info, Protocol>>,

    #[account(mut, token::mint = share_mint, token::authority = burn_authority)]
    pub burn_shares: Box<InterfaceAccount<'info, TokenAccountI>>,

    /// CHECK: address-only PDA; only the buyback instructions sign for it.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,

    #[account(mut, token::mint = share_mint, token::authority = protocol, constraint = is_ata(&treasury_shares.to_account_info(), &protocol.key(), &share_mint.key()) @ VaultError::NotBasketVault)]
    pub treasury_shares: Box<InterfaceAccount<'info, TokenAccountI>>,

    pub share_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the params account (`params.rs`): the mint switch and the transfer-fee cap.
    #[account(address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct MintInKind<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,

    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,

    #[account(mut, token::mint = share_mint, token::authority = payer)]
    pub payer_shares: InterfaceAccount<'info, TokenAccountI>,

    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,

    #[account(mut, token::mint = share_mint, token::authority = burn_authority)]
    pub burn_shares: InterfaceAccount<'info, TokenAccountI>,

    /// CHECK: address-only PDA; only the buyback instructions sign for it.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,

    #[account(mut, token::mint = share_mint, token::authority = protocol, constraint = is_ata(&treasury_shares.to_account_info(), &protocol.key(), &share_mint.key()) @ VaultError::NotBasketVault)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    /// Required only when access == Allowlist.
    pub allow_entry: Option<Account<'info, AllowEntry>>,

    pub share_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the params account (`params.rs`): the mint switch and the transfer-fee cap.
    #[account(address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct RedeemInKind<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,

    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,

    #[account(mut, token::mint = share_mint, token::authority = payer)]
    pub payer_shares: InterfaceAccount<'info, TokenAccountI>,

    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,

    #[account(mut, token::mint = share_mint, token::authority = burn_authority)]
    pub burn_shares: InterfaceAccount<'info, TokenAccountI>,

    /// CHECK: address-only PDA; only the buyback instructions sign for it.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,

    #[account(mut, token::mint = share_mint, token::authority = protocol, constraint = is_ata(&treasury_shares.to_account_info(), &protocol.key(), &share_mint.key()) @ VaultError::NotBasketVault)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
}
