//! Minting and redeeming: units of every token in, shares out, and back. No price is involved.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::state::*;
use crate::token_util::*;

/// First deposit. Fixes `units_per_share` for every sleeve and mints the opening shares.
/// Callable once, by the creator, regardless of access mode.
///
/// While the protocol asks for a creation burn, the creator burns that much GRAVI from their
/// own account here: a real burn, so GRAVI's supply falls. It's paid with the seed rather than
/// at `initialize_basket`, so a basket that's never seeded never burned anything: the two land
/// or fail together. With the amount at 0 the GRAVI accounts may be left out.
pub fn seed_basket<'info>(
    ctx: Context<'_, '_, 'info, 'info, SeedBasket<'info>>,
    shares: u64,
    units_per_share: Vec<u64>,
) -> Result<()> {
    require!(!ctx.accounts.basket.seeded, VaultError::AlreadySeeded);
    require!(!ctx.accounts.basket.mint_paused, VaultError::MintPaused);
    require!(shares > 0, VaultError::ZeroShares);
    require_keys_eq!(
        ctx.accounts.payer.key(),
        ctx.accounts.basket.creator,
        VaultError::MintNotPermitted
    );
    require!(
        units_per_share.len() == ctx.accounts.basket.constituent_count as usize,
        VaultError::BadRemainingAccounts
    );
    require!(units_per_share.iter().all(|u| *u > 0), VaultError::ZeroShares);

    for (i, u) in units_per_share.iter().enumerate() {
        ctx.accounts.basket.constituents[i].units_per_share = *u;
    }
    ctx.accounts.basket.seeded = true;
    // The creator fee is owed from when shares exist, not from when the basket was created:
    // a basket left unseeded for months would otherwise charge its first holders for them.
    ctx.accounts.basket.last_fee_accrual = Clock::get()?.unix_timestamp;

    burn_for_creation(&ctx)?;
    let c = &mut ctx.accounts.creation_burn;
    if c.amount > 0 {
        c.total_burned = c.total_burned.checked_add(c.amount).ok_or(VaultError::MathOverflow)?;
        c.baskets = c.baskets.saturating_add(1);
    }

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
        treasury_shares: &a.treasury_shares,
        share_token_program: &a.share_token_program,
    };
    collect_and_mint(deposit, ctx.remaining_accounts, shares)
}

fn burn_for_creation(ctx: &Context<SeedBasket>) -> Result<()> {
    let amount = ctx.accounts.creation_burn.amount;
    if amount == 0 {
        return Ok(());
    }
    let (Some(mint), Some(from), Some(program)) = (
        ctx.accounts.gravi_mint.as_ref(),
        ctx.accounts.creator_gravi.as_ref(),
        ctx.accounts.gravi_token_program.as_ref(),
    ) else {
        return err!(VaultError::CreationBurnAccountsMissing);
    };
    // The mint is GRAVI; the token program checks the rest as it burns: that it owns the mint
    // and the account, that the account holds GRAVI, and that the creator owns it.
    require_keys_eq!(mint.key(), ctx.accounts.protocol.gravi_mint, VaultError::GraviMintNotSet);
    token_interface::burn(
        CpiContext::new(
            program.to_account_info(),
            token_interface::Burn {
                mint: mint.to_account_info(),
                from: from.to_account_info(),
                authority: ctx.accounts.payer.to_account_info(),
            },
        ),
        amount,
    )
}

/// Deposit the exact underlying basket for up to `shares`, receive them less the
/// protocol's mint fee. No oracle is consulted.
///
/// With `min_shares` below `shares`, mints as many as the payer's token balances cover,
/// but never fewer than `min_shares`. That is how a wallet buys with swaps whose output
/// isn't known to the unit when it signs: it pre-signs this with the quoted amount and
/// its slippage floor, and nothing is left over except what legs filled unevenly.
pub fn mint_in_kind<'info>(
    ctx: Context<'_, '_, 'info, 'info, MintInKind<'info>>,
    shares: u64,
    min_shares: u64,
) -> Result<()> {
    require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
    require!(!ctx.accounts.basket.mint_paused, VaultError::MintPaused);
    require!(min_shares <= shares, VaultError::BadParameter);
    let shares = if min_shares < shares { affordable_shares(&ctx, shares)? } else { shares };
    require!(shares > 0, VaultError::ZeroShares);
    require!(shares >= min_shares, VaultError::BelowMinShares);

    let basket = &ctx.accounts.basket;
    let payer = ctx.accounts.payer.key();
    match basket.access {
        AccessMode::Public => {}
        AccessMode::Private => {
            require_keys_eq!(payer, basket.creator, VaultError::MintNotPermitted);
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

    let a = &mut *ctx.accounts;
    let deposit = Deposit {
        payer: &a.payer,
        basket: &mut a.basket,
        share_mint: &a.share_mint,
        payer_shares: &a.payer_shares,
        protocol: &a.protocol,
        burn_shares: &a.burn_shares,
        treasury_shares: &a.treasury_shares,
        share_token_program: &a.share_token_program,
    };
    collect_and_mint(deposit, ctx.remaining_accounts, shares)
}

/// Hand in `shares`: the protocol's redeem fee is kept in shares, the rest are burned
/// and their underlying paid out pro-rata. Never gated — not by access mode, not
/// by a delisting. A holder who cannot redeem holds a promise, not a claim.
///
/// `forfeit_mask` (bit i = sleeve i, normally 0) is the way out when an issuer has
/// paused or frozen one token: the redeemer leaves those sleeves behind and takes the
/// rest. What they leave stays in the vault for the remaining holders: the units are
/// re-derived from the vaults at the end, so it's theirs at once.
///
/// Payouts are priced from the vaults' balances (`Basket::live_units`), not from stored
/// units: a surplus is shared with the redeemer, a clawback's deficit is borne pro rata.
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
    require!(
        ctx.remaining_accounts.len() == count * 4,
        VaultError::BadRemainingAccounts
    );

    let fee = flow_fee(shares, ctx.accounts.protocol.protocol_fee_bps)?;
    require!(shares > fee, VaultError::BelowFee);
    let (burn_fee, treasury_fee) = split_protocol_fee(fee, ctx.accounts.protocol.burn_share_bps)?;
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
    let shares = shares - fee;

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

    let basket_key = ctx.accounts.basket.key();
    let creator = ctx.accounts.basket.creator;
    let basket_id = ctx.accounts.basket.basket_id;
    let bump = [ctx.accounts.basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let signer: &[&[&[u8]]] = &[&seeds];

    for i in 0..count {
        if forfeit_mask & (1 << i) != 0 {
            continue;
        }
        let c = ctx.accounts.basket.constituents[i];
        let vault_ata = &ctx.remaining_accounts[i * 4];
        let user_ata = &ctx.remaining_accounts[i * 4 + 1];
        let mint_ai = &ctx.remaining_accounts[i * 4 + 2];
        let token_program = &ctx.remaining_accounts[i * 4 + 3];

        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        // The mint's owner *is* its token program; anything else would let a caller
        // route the transfer through a program of their choosing.
        require_keys_eq!(*token_program.key, *mint_ai.owner, VaultError::ConstituentMismatch);

        let vault_view = read_vault(vault_ata, &basket_key, &c.mint)?;

        let units = Basket::live_units(vault_view.amount, supply_before, c.units_per_share)?;
        let amount = Basket::amount_for(units, shares, false)?;
        // Can't happen with live units (a slice of the balance, rounded down); kept as the
        // last line between a mistake and paying early redeemers out of later ones.
        require!(
            vault_view.amount >= amount,
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

    // What's left, forfeited sleeves included, backs the shares still out.
    resync_units(&mut ctx.accounts.basket, ctx.remaining_accounts, 4, &basket_key, supply_before - shares)
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
    pub treasury_shares: &'a InterfaceAccount<'info, TokenAccountI>,
    pub share_token_program: &'a Interface<'info, TokenInterface>,
}

/// Shared body of `seed_basket` and `mint_in_kind`: pull every sleeve in, then mint.
/// Remaining accounts are one `[vault_ata, user_ata, mint, token_program]` quad per
/// constituent.
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
        let units = Basket::live_units(vault_before, supply, c.units_per_share)?;
        d.basket.constituents[i].units_per_share = units;
        let amount = Basket::amount_for(units, shares, true)?;
        let decimals = mint_decimals(mint_ai)?;
        // A Token-2022 transfer fee is the payer's to bear: they send enough that the vault
        // is credited the full amount. New money stops if the fee is raised past the cap.
        let fee = TransferFee::of(mint_ai)?;
        if amount > 0 {
            fee.assert_within_cap()?;
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

    // The deposit covers all `shares`; the protocol's cut of them goes to the burn PDA
    // and the treasury, so units and collateral are untouched.
    let fee = flow_fee(shares, d.protocol.protocol_fee_bps)?;
    require!(shares > fee, VaultError::BelowFee);
    let (burn_fee, treasury_fee) = split_protocol_fee(fee, d.protocol.burn_share_bps)?;
    let program = d.share_token_program.to_account_info();
    let mint = d.share_mint.to_account_info();
    let authority = d.basket.to_account_info();
    for (to, amount) in [
        (d.payer_shares.to_account_info(), shares - fee),
        (d.burn_shares.to_account_info(), burn_fee),
        (d.treasury_shares.to_account_info(), treasury_fee),
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
    require!(ctx.remaining_accounts.len() == count * 4, VaultError::BadRemainingAccounts);
    let payer = ctx.accounts.payer.key();
    let basket_key = ctx.accounts.basket.key();
    let supply = ctx.accounts.share_mint.supply;
    let mut shares = cap as u128;
    for i in 0..count {
        let c = ctx.accounts.basket.constituents[i];
        // The same live units `collect_and_mint` will charge.
        let held = read_vault(&ctx.remaining_accounts[i * 4], &basket_key, &c.mint)?.amount;
        let units = Basket::live_units(held, supply, c.units_per_share)?;
        if units == 0 {
            continue;
        }
        let user = read_token_account(&ctx.remaining_accounts[i * 4 + 1])?;
        require_keys_eq!(user.mint, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(user.owner, payer, VaultError::ConstituentMismatch);
        let mint_ai = &ctx.remaining_accounts[i * 4 + 2];
        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        let usable = TransferFee::of(mint_ai)?.net(user.amount)?;
        shares = shares.min(usable as u128 * ONE_SHARE / units as u128);
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

/// Splits a protocol fee between the burn PDA (for buying and burning GRAVI) and the treasury.
/// The burn's cut rounds up (never past the fee), so it is at least `burn_share_bps` of every
/// fee, odd ones included: rounded down, an odd fee at the 50% floor burned less than half.
pub(crate) fn split_protocol_fee(fee: u64, burn_share_bps: u16) -> Result<(u64, u64)> {
    let burn = u64::try_from(((fee as u128) * (burn_share_bps as u128)).div_ceil(BPS_DENOM))
        .map_err(|_| VaultError::MathOverflow)?
        .min(fee);
    Ok((burn, fee - burn))
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

    #[account(mut, token::mint = share_mint, token::authority = protocol)]
    pub treasury_shares: Box<InterfaceAccount<'info, TokenAccountI>>,

    pub share_token_program: Interface<'info, TokenInterface>,

    #[account(mut, seeds = [CREATION_BURN_SEED], bump = creation_burn.bump)]
    pub creation_burn: Box<Account<'info, CreationBurn>>,
    /// CHECK: GRAVI, matched against the protocol's mint in `burn_for_creation`. Needed only
    /// while the creation burn is on.
    #[account(mut)]
    pub gravi_mint: Option<UncheckedAccount<'info>>,
    /// CHECK: the creator's GRAVI account; the token program checks it as it burns.
    #[account(mut)]
    pub creator_gravi: Option<UncheckedAccount<'info>>,
    pub gravi_token_program: Option<Interface<'info, TokenInterface>>,
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

    #[account(mut, token::mint = share_mint, token::authority = protocol)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    /// Required only when access == Allowlist.
    pub allow_entry: Option<Account<'info, AllowEntry>>,

    pub share_token_program: Interface<'info, TokenInterface>,
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

    #[account(mut, token::mint = share_mint, token::authority = protocol)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
}
