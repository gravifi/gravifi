use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::{invoke, invoke_signed};
use anchor_lang::solana_program::sysvar::instructions::{load_current_index_checked, load_instruction_at_checked};
use anchor_spl::token_2022::spl_token_2022;
use anchor_spl::token_interface::{
    self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface,
};

pub mod errors;
pub mod extensions;
pub mod state;
pub mod token_util;

use errors::VaultError;
use state::*;
use token_util::*;

declare_id!("HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G");

/// Hard ceilings on the two fees: the creator's, in bps per year, streamed while shares are
/// held; and the protocol's, in bps of the shares on every mint and every redeem. Holding
/// pays the protocol nothing.
///
/// Governance moves fees *within* these bounds; only a program upgrade can move the
/// bounds themselves. That makes the price a property of the code rather than a promise a
/// future vote could quietly break.
pub const MAX_CREATOR_FEE_BPS: u16 = 20;
pub const MAX_PROTOCOL_FEE_BPS: u16 = 50;

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

#[program]
pub mod gravifi_vault {
    use super::*;

    // ------------------------------------------------------------------ protocol

    /// One-time protocol setup, gated on the program's upgrade authority. The Protocol
    /// account is a singleton, so without that gate anyone could initialise it first and
    /// appoint themselves the authority that decides which assets exist.
    pub fn init_protocol(
        ctx: Context<InitProtocol>,
        gravi_mint: Pubkey,
        protocol_fee_bps: u16,
        burn_share_bps: u16,
    ) -> Result<()> {
        assert_upgrade_authority(
            &ctx.accounts.program_data.to_account_info(),
            &ctx.accounts.authority.key(),
        )?;
        require!(protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS, VaultError::FeeTooHigh);
        require!(burn_share_bps as u128 <= BPS_DENOM, VaultError::BadParameter);

        let p = &mut ctx.accounts.protocol;
        p.bump = ctx.bumps.protocol;
        p.authority = ctx.accounts.authority.key();
        p.guardian = Pubkey::default();
        p.gravi_mint = gravi_mint;
        p.protocol_fee_bps = protocol_fee_bps;
        p.burn_share_bps = burn_share_bps;
        p.listed_count = 0;
        Ok(())
    }

    /// Hands the protocol to a new authority, e.g. from the deployer to the team multisig.
    pub fn set_protocol_authority(ctx: Context<ProtocolAdmin>, new_authority: Pubkey) -> Result<()> {
        ctx.accounts.protocol.authority = new_authority;
        Ok(())
    }

    /// Sets the GRAVI mint, once. The token launches after the program, so `init_protocol`
    /// may leave it unset; after this it can never change.
    pub fn set_gravi_mint(ctx: Context<ProtocolAdmin>, gravi_mint: Pubkey) -> Result<()> {
        let p = &mut ctx.accounts.protocol;
        require!(p.gravi_mint == Pubkey::default(), VaultError::GraviMintAlreadySet);
        require!(gravi_mint != Pubkey::default(), VaultError::BadParameter);
        p.gravi_mint = gravi_mint;
        Ok(())
    }

    /// `Pubkey::default()` removes the guardian.
    pub fn set_guardian(ctx: Context<ProtocolAdmin>, guardian: Pubkey) -> Result<()> {
        ctx.accounts.protocol.guardian = guardian;
        Ok(())
    }

    pub fn set_protocol_fees(
        ctx: Context<ProtocolAdmin>,
        protocol_fee_bps: u16,
        burn_share_bps: u16,
    ) -> Result<()> {
        require!(protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS, VaultError::FeeTooHigh);
        require!(burn_share_bps as u128 <= BPS_DENOM, VaultError::BadParameter);
        let p = &mut ctx.accounts.protocol;
        p.protocol_fee_bps = protocol_fee_bps;
        p.burn_share_bps = burn_share_bps;
        Ok(())
    }

    /// Adds a mint to the set of assets baskets may be built from.
    pub fn register_asset(ctx: Context<RegisterAsset>) -> Result<()> {
        assert_no_active_transfer_hook(&ctx.accounts.mint.to_account_info())?;
        TransferFee::of(&ctx.accounts.mint.to_account_info())?.assert_within_cap()?;

        let e = &mut ctx.accounts.asset_entry;
        e.bump = ctx.bumps.asset_entry;
        e.mint = ctx.accounts.mint.key();
        e.status = AssetStatus::Listed;
        e.listed_at = Clock::get()?.unix_timestamp;
        e.delisted_at = 0;

        let p = &mut ctx.accounts.protocol;
        p.listed_count = p.listed_count.checked_add(1).ok_or(VaultError::MathOverflow)?;
        Ok(())
    }

    /// Relists or delists a registered asset.
    ///
    /// The guardian may delist but never list, and the asymmetry is the point. Listing is
    /// the action worth bribing a vote for, so it must go through governance. Delisting is
    /// the action that has to happen in minutes when a listed token turns out to be
    /// compromised, so it cannot wait out a vote and a timelock.
    pub fn set_asset_status(ctx: Context<SetAssetStatus>, status: AssetStatus) -> Result<()> {
        let signer = ctx.accounts.signer.key();
        let p = &ctx.accounts.protocol;
        let is_authority = signer == p.authority;
        let is_guardian = p.guardian != Pubkey::default() && signer == p.guardian;

        match status {
            AssetStatus::Listed => {
                require!(is_authority, VaultError::NotProtocolAuthority);
                assert_no_active_transfer_hook(&ctx.accounts.mint.to_account_info())?;
            }
            AssetStatus::Delisted => {
                require!(is_authority || is_guardian, VaultError::NotProtocolAuthority);
            }
        }

        let now = Clock::get()?.unix_timestamp;
        let e = &mut ctx.accounts.asset_entry;
        if e.status == status {
            return Ok(());
        }
        e.status = status;
        let p = &mut ctx.accounts.protocol;
        match status {
            AssetStatus::Listed => {
                e.listed_at = now;
                p.listed_count = p.listed_count.checked_add(1).ok_or(VaultError::MathOverflow)?;
            }
            AssetStatus::Delisted => {
                e.delisted_at = now;
                p.listed_count = p.listed_count.saturating_sub(1);
            }
        }
        Ok(())
    }

    /// Moves protocol-treasury shares. Burn-earmarked shares have no equivalent: the burn
    /// PDA only signs inside the buyback instructions, so they can only ever be redeemed
    /// into the burn PDA's own accounts and sold for GRAVI that is burned.
    pub fn treasury_withdraw(ctx: Context<TreasuryWithdraw>, amount: u64) -> Result<()> {
        let bump = [ctx.accounts.protocol.bump];
        let seeds: [&[u8]; 2] = [PROTOCOL_SEED, &bump];
        let signer: &[&[&[u8]]] = &[&seeds];

        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.share_token_program.to_account_info(),
                token_interface::TransferChecked {
                    from: ctx.accounts.treasury_shares.to_account_info(),
                    mint: ctx.accounts.share_mint.to_account_info(),
                    to: ctx.accounts.destination.to_account_info(),
                    authority: ctx.accounts.protocol.to_account_info(),
                },
                signer,
            ),
            amount,
            ctx.accounts.share_mint.decimals,
        )
    }

    // ------------------------------------------------------------------ baskets

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

    /// First deposit. Fixes `units_per_share` for every sleeve and mints the opening shares.
    /// Callable once, by the creator, regardless of access mode.
    pub fn seed_basket<'info>(
        ctx: Context<'_, '_, 'info, 'info, MintInKind<'info>>,
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

        // The opening deposit pays the fee like any other: fee-free, a basket of one's own
        // would be the way to buy in without it.
        collect_and_mint(ctx, shares)
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

        collect_and_mint(ctx, shares)
    }

    /// Hand in `shares`: the protocol's redeem fee is kept in shares, the rest are burned
    /// and their underlying paid out pro-rata. Never gated — not by access mode, not
    /// by a delisting. A holder who cannot redeem holds a promise, not a claim.
    ///
    /// `forfeit_mask` (bit i = sleeve i, normally 0) is the way out when an issuer has
    /// paused or frozen one token: the redeemer leaves those sleeves behind and takes the
    /// rest. What they leave stays in the vault for the remaining holders (`sync_units`
    /// credits it), so no one else is short.
    pub fn redeem_in_kind<'info>(
        ctx: Context<'_, '_, 'info, 'info, RedeemInKind<'info>>,
        shares: u64,
        forfeit_mask: u16,
    ) -> Result<()> {
        require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
        require!(shares > 0, VaultError::ZeroShares);

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

            let amount = Basket::amount_for(c.units_per_share, shares, false)?;
            // The units promise can exceed reality if an issuer's permanent delegate clawed
            // tokens back. Fail loudly and point at the fix rather than paying early
            // redeemers out of later ones.
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

        Ok(())
    }

    /// Streams the creator's fee as share dilution. (The protocol takes nothing from
    /// holders: its fee is charged on mint and redeem.)
    ///
    /// Minting fee shares without depositing assets would leave `units * supply` above what
    /// the vault holds, so units are re-derived from the vault balances and the new supply in
    /// the same instruction. The total claim on the assets is unchanged; each share's slice
    /// shrinks.
    ///
    /// Re-derived, not scaled: scaling each unit by `old / new` and rounding down lost about
    /// a whole unit at every accrual, however small the fee, and for a sleeve worth few
    /// units a share (wBTC: ~30,000) that compounded to a vault holding a growing surplus
    /// that redemptions never paid out.
    ///
    /// Remaining accounts: each constituent's vault, in basket order.
    pub fn accrue_fees<'info>(ctx: Context<'_, '_, 'info, 'info, AccrueFees<'info>>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);

        let elapsed = (now - ctx.accounts.basket.last_fee_accrual).max(0);
        let supply = ctx.accounts.share_mint.supply;
        require!(supply > 0, VaultError::ZeroSupply);

        let creator_fee = streamed_fee(supply, ctx.accounts.basket.mgmt_fee_bps, elapsed)?;

        // Leave the clock alone when nothing was minted. Advancing it anyway would let
        // anyone call this every slot, round each period's fee down to zero, and erase
        // the fee entirely.
        if creator_fee == 0 {
            return Ok(());
        }

        let creator = ctx.accounts.basket.creator;
        let basket_id = ctx.accounts.basket.basket_id;
        let bump = [ctx.accounts.basket.bump];
        let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
        let signer: &[&[&[u8]]] = &[&seeds];

        mint_shares(
            &ctx.accounts.share_token_program.to_account_info(),
            &ctx.accounts.share_mint.to_account_info(),
            &ctx.accounts.creator_shares.to_account_info(),
            &ctx.accounts.basket.to_account_info(),
            signer,
            creator_fee,
        )?;

        let new_supply = supply.checked_add(creator_fee).ok_or(VaultError::MathOverflow)?;

        let count = ctx.accounts.basket.constituent_count as usize;
        require!(ctx.remaining_accounts.len() == count, VaultError::BadRemainingAccounts);
        let basket_key = ctx.accounts.basket.key();
        let basket = &mut ctx.accounts.basket;
        for i in 0..count {
            let held = read_vault(&ctx.remaining_accounts[i], &basket_key, &basket.constituents[i].mint)?.amount;
            let units = (held as u128)
                .checked_mul(ONE_SHARE)
                .ok_or(VaultError::MathOverflow)?
                / new_supply as u128;
            basket.constituents[i].units_per_share = u64::try_from(units).map_err(|_| VaultError::MathOverflow)?;
        }
        basket.last_fee_accrual = now;

        Ok(())
    }

    /// Permissionless reconciliation of the units promise against what the vault holds.
    ///
    /// Balances move without the vault's participation: a ScaledUiAmount dividend credits
    /// the vault (the surplus belongs to holders and should raise units), and a
    /// PermanentDelegate clawback debits it (the deficit must lower units, or redemption
    /// becomes first-come-first-served). It only ever makes the promise match reality.
    pub fn sync_units<'info>(
        ctx: Context<'_, '_, 'info, 'info, SyncUnits<'info>>,
    ) -> Result<()> {
        let count = ctx.accounts.basket.constituent_count as usize;
        require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
        require!(
            ctx.remaining_accounts.len() == count,
            VaultError::BadRemainingAccounts
        );

        let supply = ctx.accounts.share_mint.supply;
        require!(supply > 0, VaultError::ZeroSupply);
        let basket_key = ctx.accounts.basket.key();

        for i in 0..count {
            let view = read_vault(&ctx.remaining_accounts[i], &basket_key, &ctx.accounts.basket.constituents[i].mint)?;

            let units = (view.amount as u128)
                .checked_mul(ONE_SHARE)
                .and_then(|v| v.checked_div(supply as u128))
                .ok_or(VaultError::MathOverflow)?;
            ctx.accounts.basket.constituents[i].units_per_share =
                u64::try_from(units).map_err(|_| VaultError::MathOverflow)?;
        }

        Ok(())
    }

    /// Permissionless: pauses minting if any constituent is delisted, and unpauses once
    /// all of them are listed again. Remaining accounts are the constituents' asset
    /// entries, in order.
    pub fn sync_listing_status<'info>(
        ctx: Context<'_, '_, 'info, 'info, SyncListing<'info>>,
    ) -> Result<()> {
        let count = ctx.accounts.basket.constituent_count as usize;
        require!(
            ctx.remaining_accounts.len() == count,
            VaultError::BadRemainingAccounts
        );

        let mut any_delisted = false;
        for i in 0..count {
            let entry = load_asset_entry(&ctx.remaining_accounts[i])?;
            require_keys_eq!(
                entry.mint,
                ctx.accounts.basket.constituents[i].mint,
                VaultError::ConstituentMismatch
            );
            any_delisted |= entry.status == AssetStatus::Delisted;
        }
        ctx.accounts.basket.mint_paused = any_delisted;
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

    // ------------------------------------------------------------------ keepers and prices

    /// Names who may post prices and who may run rebalances. Protocol authority only.
    pub fn init_keepers(ctx: Context<InitKeepers>, price_authority: Pubkey, rebalancer: Pubkey) -> Result<()> {
        let k = &mut ctx.accounts.keepers;
        k.bump = ctx.bumps.keepers;
        k.price_authority = price_authority;
        k.rebalancer = rebalancer;
        Ok(())
    }

    pub fn set_keepers(ctx: Context<SetKeepers>, price_authority: Pubkey, rebalancer: Pubkey) -> Result<()> {
        let k = &mut ctx.accounts.keepers;
        k.price_authority = price_authority;
        k.rebalancer = rebalancer;
        Ok(())
    }

    /// Posts an asset's USD price (per whole token, x1e12). Testnet oracle; on mainnet the
    /// rebalancer reads Pyth instead.
    pub fn set_price(ctx: Context<SetPrice>, price_e12: u64) -> Result<()> {
        require!(price_e12 > 0, VaultError::BadPrice);
        let decimals = mint_decimals(&ctx.accounts.mint.to_account_info())?;
        let p = &mut ctx.accounts.asset_price;
        p.bump = ctx.bumps.asset_price;
        p.mint = ctx.accounts.mint.key();
        p.decimals = decimals;
        p.price_e12 = price_e12;
        p.updated_at = Clock::get()?.unix_timestamp;
        Ok(())
    }

    // ------------------------------------------------------------------------ rebalancing

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
        require!(due || ctx.accounts.rebalance_state.session_active || leaving || joining, VaultError::RebalanceNotDue);
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
        let buy_fee = TransferFee::of(&ctx.accounts.buy_mint.to_account_info())?;
        buy_fee.assert_within_cap()?;
        let floor_value = sell_value * (BPS_DENOM - max_slippage_bps as u128) / BPS_DENOM * (BPS_DENOM - sell_fee.bps as u128) / BPS_DENOM
            * (BPS_DENOM - buy_fee.bps as u128)
            / BPS_DENOM;
        let min_buy = u64::try_from(
            floor_value
                .checked_mul(10u128.pow(book.rows[b].decimals as u32))
                .ok_or(VaultError::MathOverflow)?
                / book.rows[b].price_e12 as u128,
        )
        .map_err(|_| VaultError::MathOverflow)?;

        // The finish must come later in this very transaction, for this basket.
        let ixs = ctx.accounts.instructions.to_account_info();
        let current = load_current_index_checked(&ixs)? as usize;
        let basket_key = basket.key();
        let mut found = false;
        let mut i = current + 1;
        while let Ok(ix) = load_instruction_at_checked(i, &ixs) {
            if ix.program_id == crate::ID
                && ix.data.len() >= 8
                && ix.data[..8] == *<crate::instruction::RebalanceFinish as anchor_lang::Discriminator>::DISCRIMINATOR
                && ix.accounts.get(2).map(|a| a.pubkey) == Some(basket_key)
            {
                found = true;
                break;
            }
            i += 1;
        }
        require!(found, VaultError::FinishMissing);

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

    /// Second half of a slice: checks the oracle floor, re-derives units for the two
    /// sleeves that traded, and ends the session once every sleeve is near target.
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
        let settled = drift.iter().all(|d| d.abs() < SESSION_END_DRIFT_BPS);

        let supply = ctx.accounts.share_mint.supply;
        require!(supply > 0, VaultError::ZeroSupply);
        require!(supply == ctx.accounts.basket.rebalance_supply, VaultError::SupplyChanged);
        let basket = &mut ctx.accounts.basket;
        for i in [s, b] {
            let units = (book.rows[i].balance as u128)
                .checked_mul(ONE_SHARE)
                .ok_or(VaultError::MathOverflow)?
                / supply as u128;
            basket.constituents[i].units_per_share = u64::try_from(units).map_err(|_| VaultError::MathOverflow)?;
        }

        let st = &mut ctx.accounts.rebalance_state;
        st.pending = false;
        st.slices = st.slices.saturating_add(1);
        if settled {
            st.session_active = false;
            basket.last_rebalance = now;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ buyback and burn

    /// Turns burn-earmarked shares of one basket into its underlying tokens, held by the
    /// burn PDA, ready to be sold for GRAVI. No fee, no price: exactly what a redeem pays.
    /// Permissionless, since value only moves from the basket to the burn PDA.
    ///
    /// Remaining accounts: one `[vault_ata, burn_ata, mint, token_program]` quad per
    /// constituent, `burn_ata` being a token account the burn PDA owns.
    pub fn buyback_redeem<'info>(
        ctx: Context<'_, '_, 'info, 'info, BuybackRedeem<'info>>,
        shares: u64,
    ) -> Result<()> {
        require!(ctx.accounts.basket.seeded, VaultError::NotSeeded);
        require!(shares > 0, VaultError::ZeroShares);
        let count = ctx.accounts.basket.constituent_count as usize;
        require!(ctx.remaining_accounts.len() == count * 4, VaultError::BadRemainingAccounts);

        let burn_bump = [ctx.bumps.burn_authority];
        let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
        token_interface::burn(
            CpiContext::new_with_signer(
                ctx.accounts.share_token_program.to_account_info(),
                token_interface::Burn {
                    mint: ctx.accounts.share_mint.to_account_info(),
                    from: ctx.accounts.burn_shares.to_account_info(),
                    authority: ctx.accounts.burn_authority.to_account_info(),
                },
                &[&burn_seeds],
            ),
            shares,
        )?;

        let basket_key = ctx.accounts.basket.key();
        let burn_key = ctx.accounts.burn_authority.key();
        let creator = ctx.accounts.basket.creator;
        let basket_id = ctx.accounts.basket.basket_id;
        let bump = [ctx.accounts.basket.bump];
        let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
        for i in 0..count {
            let c = ctx.accounts.basket.constituents[i];
            let vault_ata = &ctx.remaining_accounts[i * 4];
            let burn_ata = &ctx.remaining_accounts[i * 4 + 1];
            let mint_ai = &ctx.remaining_accounts[i * 4 + 2];
            let token_program = &ctx.remaining_accounts[i * 4 + 3];
            require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
            require_keys_eq!(*token_program.key, *mint_ai.owner, VaultError::ConstituentMismatch);
            let vault_view = read_vault(vault_ata, &basket_key, &c.mint)?;
            let burn_view = read_token_account(burn_ata)?;
            require_keys_eq!(burn_view.mint, c.mint, VaultError::ConstituentMismatch);
            require_keys_eq!(burn_view.owner, burn_key, VaultError::ConstituentMismatch);

            let amount = Basket::amount_for(c.units_per_share, shares, false)?;
            require!(vault_view.amount >= amount, VaultError::VaultUndercollateralised);
            transfer_from_vault(
                mint_ai,
                vault_ata,
                burn_ata,
                &ctx.accounts.basket.to_account_info(),
                token_program,
                amount,
                mint_decimals(mint_ai)?,
                &[&seeds],
            )?;
        }
        Ok(())
    }

    /// First half of a buyback slice: lends `sell_amount` of a token the burn PDA holds to
    /// the rebalancer, who swaps it for GRAVI in the same transaction. The matching
    /// `buyback_finish` must follow and checks at least the oracle value, less
    /// `max_slippage_bps`, came back as GRAVI.
    pub fn buyback_start(ctx: Context<BuybackStart>, sell_amount: u64, max_slippage_bps: u16) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(sell_amount > 0, VaultError::BadParameter);
        require!(max_slippage_bps <= MAX_BUYBACK_SLIPPAGE_BPS, VaultError::BadParameter);
        require!(!ctx.accounts.buyback.pending, VaultError::BuybackPending);
        let gravi_mint = ctx.accounts.protocol.gravi_mint;
        require!(gravi_mint != Pubkey::default(), VaultError::GraviMintNotSet);
        require_keys_neq!(ctx.accounts.sell_mint.key(), gravi_mint, VaultError::BadParameter);

        let sell_price = &ctx.accounts.sell_price;
        let gravi_price = &ctx.accounts.gravi_price;
        for p in [sell_price, gravi_price] {
            require!(p.price_e12 > 0 && now - p.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
        }
        // As in rebalancing, the sold token's transfer fee isn't slippage.
        let sell_fee = TransferFee::of(&ctx.accounts.sell_mint.to_account_info())?;
        let floor_value = token_value(sell_amount, sell_price.price_e12, sell_price.decimals)?
            * (BPS_DENOM - max_slippage_bps as u128)
            / BPS_DENOM
            * (BPS_DENOM - sell_fee.bps as u128)
            / BPS_DENOM;
        let min_gravi = u64::try_from(
            floor_value
                .checked_mul(10u128.pow(gravi_price.decimals as u32))
                .ok_or(VaultError::MathOverflow)?
                / gravi_price.price_e12 as u128,
        )
        .map_err(|_| VaultError::MathOverflow)?;

        // The finish must come later in this very transaction.
        let ixs = ctx.accounts.instructions.to_account_info();
        let mut i = load_current_index_checked(&ixs)? as usize + 1;
        let mut found = false;
        while let Ok(ix) = load_instruction_at_checked(i, &ixs) {
            if ix.program_id == crate::ID
                && ix.data.len() >= 8
                && ix.data[..8] == *<crate::instruction::BuybackFinish as anchor_lang::Discriminator>::DISCRIMINATOR
            {
                found = true;
                break;
            }
            i += 1;
        }
        require!(found, VaultError::BuybackFinishMissing);

        let burn_bump = [ctx.bumps.burn_authority];
        let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.sell_token_program.to_account_info(),
                token_interface::TransferChecked {
                    from: ctx.accounts.sell_source.to_account_info(),
                    mint: ctx.accounts.sell_mint.to_account_info(),
                    to: ctx.accounts.rebalancer_sell.to_account_info(),
                    authority: ctx.accounts.burn_authority.to_account_info(),
                },
                &[&burn_seeds],
            ),
            sell_amount,
            ctx.accounts.sell_mint.decimals,
        )?;

        // Start and finish must read the same account: the burn PDA's canonical one.
        read_vault(&ctx.accounts.burn_gravi.to_account_info(), &ctx.accounts.burn_authority.key(), &gravi_mint)?;

        let b = &mut ctx.accounts.buyback;
        b.bump = ctx.bumps.buyback;
        b.pending = true;
        b.pending_slot = Clock::get()?.slot;
        b.min_gravi = min_gravi;
        b.gravi_before = ctx.accounts.burn_gravi.amount;
        Ok(())
    }

    /// Second half of a buyback slice: checks the GRAVI that came back clears the oracle
    /// floor, then burns every GRAVI the burn PDA holds.
    pub fn buyback_finish(ctx: Context<BuybackFinish>) -> Result<()> {
        let b = &ctx.accounts.buyback;
        require!(b.pending && b.pending_slot == Clock::get()?.slot, VaultError::NoBuybackPending);
        read_vault(&ctx.accounts.burn_gravi.to_account_info(), &ctx.accounts.burn_authority.key(), &ctx.accounts.gravi_mint.key())?;
        let held = ctx.accounts.burn_gravi.amount;
        require!(held.saturating_sub(b.gravi_before) >= b.min_gravi, VaultError::BelowOracleFloor);

        let burn_bump = [ctx.bumps.burn_authority];
        let burn_seeds: [&[u8]; 2] = [BURN_SEED, &burn_bump];
        token_interface::burn(
            CpiContext::new_with_signer(
                ctx.accounts.gravi_token_program.to_account_info(),
                token_interface::Burn {
                    mint: ctx.accounts.gravi_mint.to_account_info(),
                    from: ctx.accounts.burn_gravi.to_account_info(),
                    authority: ctx.accounts.burn_authority.to_account_info(),
                },
                &[&burn_seeds],
            ),
            held,
        )?;

        let b = &mut ctx.accounts.buyback;
        b.pending = false;
        b.total_burned = b.total_burned.checked_add(held).ok_or(VaultError::MathOverflow)?;
        b.slices = b.slices.saturating_add(1);
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

    // ----------------------------------------------------------------------------- splits

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
}

/// A schedule (unless drift-only) of at least a day, and a drift tolerance (unless
/// schedule-only) of at least 0.5%.
fn check_policy(mode: RebalanceMode, interval_secs: i64, drift_bps: u16) -> Result<()> {
    if mode != RebalanceMode::Drift {
        require!(interval_secs >= MIN_POLICY_INTERVAL_SECS, VaultError::BadPolicy);
    }
    if mode != RebalanceMode::Periodic {
        require!(drift_bps >= MIN_POLICY_DRIFT_BPS && (drift_bps as u128) <= BPS_DENOM, VaultError::BadPolicy);
    }
    Ok(())
}

fn authorise_mandate(basket: &Account<Basket>, authority: &Signer) -> Result<()> {
    match basket.control {
        ControlMode::Immutable => Err(VaultError::BasketImmutable.into()),
        ControlMode::Creator => {
            require_keys_eq!(authority.key(), basket.creator, VaultError::NotMandateAuthority);
            Ok(())
        }
        ControlMode::Dao => Err(VaultError::GovernanceRemoved.into()),
    }
}

/// Checks `expected` is the upgrade authority recorded in this program's ProgramData.
///
/// Parsed by hand rather than through `Account<ProgramData>`, which drags a bincode
/// deserialiser into the binary — tens of KB of rent to read one field. The layout is the
/// loader's fixed bincode encoding of `UpgradeableLoaderState::ProgramData`:
/// `u32` variant (3), `u64` slot, `u8` option tag, then the 32-byte authority.
fn assert_upgrade_authority(program_data: &AccountInfo, expected: &Pubkey) -> Result<()> {
    require_keys_eq!(
        *program_data.owner,
        anchor_lang::solana_program::bpf_loader_upgradeable::ID,
        VaultError::NotProtocolAuthority
    );
    let data = program_data.try_borrow_data()?;
    require!(data.len() >= 45, VaultError::NotProtocolAuthority);
    require!(data[0..4] == 3u32.to_le_bytes(), VaultError::NotProtocolAuthority);
    require!(data[12] == 1, VaultError::NotProtocolAuthority);
    require!(&data[13..45] == expected.as_ref(), VaultError::NotProtocolAuthority);
    Ok(())
}

/// Reads an AssetEntry from an arbitrary account.
///
/// Owner plus discriminator is sufficient provenance: only `register_asset` creates
/// AssetEntry accounts, always at `[ASSET_SEED, mint]`, so there is exactly one per mint
/// and callers additionally check `entry.mint` against the mint they expect.
fn load_asset_entry(ai: &AccountInfo) -> Result<AssetEntry> {
    require_keys_eq!(*ai.owner, crate::ID, VaultError::AssetNotListed);
    let data = ai.try_borrow_data()?;
    AssetEntry::try_deserialize(&mut &data[..])
}

fn streamed_fee(supply: u64, bps: u16, elapsed: i64) -> Result<u64> {
    let v = (supply as u128)
        .checked_mul(bps as u128)
        .and_then(|v| v.checked_mul(elapsed.max(0) as u128))
        .ok_or(VaultError::MathOverflow)?
        / (BPS_DENOM * SECONDS_PER_YEAR as u128);
    u64::try_from(v).map_err(|_| VaultError::MathOverflow.into())
}

fn mint_shares<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    signer: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    token_interface::mint_to(
        CpiContext::new_with_signer(
            token_program.clone(),
            token_interface::MintTo {
                mint: mint.clone(),
                to: to.clone(),
                authority: authority.clone(),
            },
            signer,
        ),
        amount,
    )
}

/// Shared body of `seed_basket` and `mint_in_kind`: pull every sleeve in, then mint.
/// Remaining accounts are one `[vault_ata, user_ata, mint, token_program]` quad per
/// constituent.
fn collect_and_mint<'info>(ctx: Context<'_, '_, 'info, 'info, MintInKind<'info>>, shares: u64) -> Result<()> {
    let count = ctx.accounts.basket.constituent_count as usize;
    require!(
        ctx.remaining_accounts.len() == count * 4,
        VaultError::BadRemainingAccounts
    );

    let basket_key = ctx.accounts.basket.key();
    let payer_key = ctx.accounts.payer.key();

    for i in 0..count {
        let c = ctx.accounts.basket.constituents[i];
        let vault_ata = &ctx.remaining_accounts[i * 4];
        let user_ata = &ctx.remaining_accounts[i * 4 + 1];
        let mint_ai = &ctx.remaining_accounts[i * 4 + 2];
        let token_program = &ctx.remaining_accounts[i * 4 + 3];

        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(*token_program.key, *mint_ai.owner, VaultError::ConstituentMismatch);

        let vault_before = read_vault(vault_ata, &basket_key, &c.mint)?.amount;

        let user_view = read_token_account(user_ata)?;
        require_keys_eq!(user_view.mint, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(user_view.owner, payer_key, VaultError::ConstituentMismatch);

        let amount = Basket::amount_for(c.units_per_share, shares, true)?;
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
            &ctx.accounts.payer.to_account_info(),
            token_program,
            fee.gross(amount)?,
            decimals,
        )?;
        // Measured, not assumed: whatever the fee did, the vault must hold the full amount.
        let received = read_token_account(vault_ata)?.amount.saturating_sub(vault_before);
        require!(received >= amount, VaultError::DepositShort);
    }

    let creator = ctx.accounts.basket.creator;
    let basket_id = ctx.accounts.basket.basket_id;
    let bump = [ctx.accounts.basket.bump];
    let seeds: [&[u8]; 4] = [b"basket", creator.as_ref(), basket_id.as_ref(), &bump];
    let signer: &[&[&[u8]]] = &[&seeds];

    // The deposit covers all `shares`; the protocol's cut of them goes to the burn PDA
    // and the treasury, so units and collateral are untouched.
    let fee = flow_fee(shares, ctx.accounts.protocol.protocol_fee_bps)?;
    require!(shares > fee, VaultError::BelowFee);
    let (burn_fee, treasury_fee) = split_protocol_fee(fee, ctx.accounts.protocol.burn_share_bps)?;
    let program = ctx.accounts.share_token_program.to_account_info();
    let mint = ctx.accounts.share_mint.to_account_info();
    let authority = ctx.accounts.basket.to_account_info();
    for (to, amount) in [
        (ctx.accounts.payer_shares.to_account_info(), shares - fee),
        (ctx.accounts.burn_shares.to_account_info(), burn_fee),
        (ctx.accounts.treasury_shares.to_account_info(), treasury_fee),
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
fn affordable_shares<'info>(ctx: &Context<'_, '_, 'info, 'info, MintInKind<'info>>, cap: u64) -> Result<u64> {
    let count = ctx.accounts.basket.constituent_count as usize;
    require!(ctx.remaining_accounts.len() == count * 4, VaultError::BadRemainingAccounts);
    let payer = ctx.accounts.payer.key();
    let mut shares = cap as u128;
    for i in 0..count {
        let c = ctx.accounts.basket.constituents[i];
        if c.units_per_share == 0 {
            continue;
        }
        let user = read_token_account(&ctx.remaining_accounts[i * 4 + 1])?;
        require_keys_eq!(user.mint, c.mint, VaultError::ConstituentMismatch);
        require_keys_eq!(user.owner, payer, VaultError::ConstituentMismatch);
        let mint_ai = &ctx.remaining_accounts[i * 4 + 2];
        require_keys_eq!(*mint_ai.key, c.mint, VaultError::ConstituentMismatch);
        let usable = TransferFee::of(mint_ai)?.net(user.amount)?;
        shares = shares.min(usable as u128 * ONE_SHARE / c.units_per_share as u128);
    }
    Ok(shares as u64)
}

/// The protocol's cut of a seed, mint or redeem, rounded up. Rounded down, any flow under
/// 10,000 / fee_bps raw units paid nothing, and a basket seeded with few raw units for a lot
/// of tokens made such a flow worth thousands: minted and redeemed in small enough pieces,
/// it never paid the fee at all.
fn flow_fee(shares: u64, fee_bps: u16) -> Result<u64> {
    let fee = ((shares as u128) * (fee_bps as u128)).div_ceil(BPS_DENOM);
    u64::try_from(fee).map_err(|_| VaultError::MathOverflow.into())
}

/// Splits a protocol fee between the burn PDA (for buying and burning GRAVI) and the treasury.
fn split_protocol_fee(fee: u64, burn_share_bps: u16) -> Result<(u64, u64)> {
    let burn = u64::try_from((fee as u128) * (burn_share_bps as u128) / BPS_DENOM).map_err(|_| VaultError::MathOverflow)?;
    Ok((burn, fee - burn))
}

// ---------------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct InitProtocol<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + Protocol::INIT_SPACE,
        seeds = [PROTOCOL_SEED],
        bump
    )]
    pub protocol: Account<'info, Protocol>,

    /// CHECK: must be this program's ProgramData account; the upgrade authority is read
    /// from it by hand in `assert_upgrade_authority`.
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = anchor_lang::solana_program::bpf_loader_upgradeable::ID
    )]
    pub program_data: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ProtocolAdmin<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol.bump,
        has_one = authority @ VaultError::NotProtocolAuthority
    )]
    pub protocol: Account<'info, Protocol>,
}

#[derive(Accounts)]
pub struct RegisterAsset<'info> {
    /// Pays the entry's rent. Separate from `authority`, which is usually a multisig.
    #[account(mut)]
    pub payer: Signer<'info>,

    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol.bump,
        has_one = authority @ VaultError::NotProtocolAuthority
    )]
    pub protocol: Account<'info, Protocol>,

    pub mint: InterfaceAccount<'info, MintI>,

    #[account(
        init,
        payer = payer,
        space = 8 + AssetEntry::INIT_SPACE,
        seeds = [ASSET_SEED, mint.key().as_ref()],
        bump
    )]
    pub asset_entry: Account<'info, AssetEntry>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetAssetStatus<'info> {
    /// Authority or guardian; which one is allowed depends on the requested status.
    pub signer: Signer<'info>,

    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,

    pub mint: InterfaceAccount<'info, MintI>,

    #[account(
        mut,
        seeds = [ASSET_SEED, mint.key().as_ref()],
        bump = asset_entry.bump
    )]
    pub asset_entry: Account<'info, AssetEntry>,
}

#[derive(Accounts)]
pub struct TreasuryWithdraw<'info> {
    pub authority: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol.bump,
        has_one = authority @ VaultError::NotProtocolAuthority
    )]
    pub protocol: Account<'info, Protocol>,

    pub share_mint: InterfaceAccount<'info, MintI>,

    #[account(mut, token::mint = share_mint, token::authority = protocol)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    #[account(mut, token::mint = share_mint)]
    pub destination: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
}

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

#[derive(Accounts)]
pub struct AccrueFees<'info> {
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,

    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,

    /// Pinned to the creator. This instruction is permissionless, so without the
    /// authority check any caller could route the creator's fee into their own account.
    #[account(mut, token::mint = share_mint, token::authority = basket.creator)]
    pub creator_shares: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct SyncUnits<'info> {
    #[account(mut, has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    pub share_mint: InterfaceAccount<'info, MintI>,
}

#[derive(Accounts)]
pub struct SyncListing<'info> {
    #[account(mut)]
    pub basket: Account<'info, Basket>,
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


// --------------------------------------------------------------------- share mint creation

fn create_share_mint(ctx: &Context<InitializeBasket>) -> Result<()> {
    let token_program = ctx.accounts.share_token_program.key();
    let t22 = token_program == anchor_spl::token_2022::ID;
    require!(t22 || token_program == anchor_spl::token::ID, VaultError::BadParameter);
    let space = if t22 { extensions::MINT_WITH_SCALED_UI_LEN } else { extensions::PLAIN_MINT_LEN };
    let basket_key = ctx.accounts.basket.key();
    let bump = [ctx.bumps.share_mint];
    let seeds: [&[u8]; 3] = [b"share", basket_key.as_ref(), &bump];
    anchor_lang::system_program::create_account(
        CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            anchor_lang::system_program::CreateAccount {
                from: ctx.accounts.creator.to_account_info(),
                to: ctx.accounts.share_mint.to_account_info(),
            },
            &[&seeds],
        ),
        Rent::get()?.minimum_balance(space),
        space as u64,
        &token_program,
    )?;
    let mint = ctx.accounts.share_mint.to_account_info();
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

// --------------------------------------------------------------------------- valuation

struct Row {
    balance: u64,
    price_e12: u64,
    decimals: u8,
    value: u128,
}

struct Book {
    rows: Vec<Row>,
    /// Basket value in USD x1e12.
    total: u128,
}

/// USD x1e12 of `amount` base units at `price_e12` per whole token.
fn token_value(amount: u64, price_e12: u64, decimals: u8) -> Result<u128> {
    Ok((amount as u128)
        .checked_mul(price_e12 as u128)
        .ok_or(VaultError::MathOverflow)?
        / 10u128.pow(decimals as u32))
}

/// Values every sleeve from its vault balance and a fresh oracle price.
/// `accounts` holds [vault_ata, asset_price] per constituent, in basket order.
fn value_book<'info>(basket: &Account<'info, Basket>, accounts: &'info [AccountInfo<'info>], now: i64) -> Result<Book> {
    let n = basket.constituent_count as usize;
    require!(accounts.len() == n * 2, VaultError::BadRemainingAccounts);
    let basket_key = basket.key();
    let mut rows = Vec::with_capacity(n);
    let mut total: u128 = 0;
    for i in 0..n {
        let c = basket.constituents[i];
        let view = read_vault(&accounts[i * 2], &basket_key, &c.mint)?;
        let price = Account::<AssetPrice>::try_from(&accounts[i * 2 + 1]).map_err(|_| error!(VaultError::BadPrice))?;
        require_keys_eq!(price.mint, c.mint, VaultError::BadPrice);
        require!(price.price_e12 > 0 && now - price.updated_at <= MAX_PRICE_AGE_SECS, VaultError::BadPrice);
        let value = token_value(view.amount, price.price_e12, price.decimals)?;
        total = total.checked_add(value).ok_or(VaultError::MathOverflow)?;
        rows.push(Row { balance: view.amount, price_e12: price.price_e12, decimals: price.decimals, value });
    }
    require!(total > 0, VaultError::ZeroSupply);
    Ok(Book { rows, total })
}

/// Each sleeve's actual weight minus its target, in bps.
fn drift_bps(basket: &Basket, book: &Book) -> Result<Vec<i64>> {
    Ok(book
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.value * BPS_DENOM / book.total) as i64 - basket.constituents[i].target_bps as i64)
        .collect())
}

// -------------------------------------------------------------------- new account structs

#[derive(Accounts)]
pub struct InitKeepers<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    #[account(init, payer = authority, space = 8 + Keepers::INIT_SPACE, seeds = [KEEPERS_SEED], bump)]
    pub keepers: Account<'info, Keepers>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetKeepers<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    #[account(mut, seeds = [KEEPERS_SEED], bump = keepers.bump)]
    pub keepers: Account<'info, Keepers>,
}

#[derive(Accounts)]
pub struct SetPrice<'info> {
    #[account(mut)]
    pub price_authority: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.price_authority == price_authority.key() @ VaultError::NotPriceAuthority)]
    pub keepers: Account<'info, Keepers>,
    pub mint: InterfaceAccount<'info, MintI>,
    #[account(init_if_needed, payer = price_authority, space = 8 + AssetPrice::INIT_SPACE, seeds = [PRICE_SEED, mint.key().as_ref()], bump)]
    pub asset_price: Account<'info, AssetPrice>,
    pub system_program: Program<'info, System>,
}

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
pub struct BuybackRedeem<'info> {
    #[account(has_one = share_mint)]
    pub basket: Account<'info, Basket>,
    #[account(mut)]
    pub share_mint: InterfaceAccount<'info, MintI>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mut, token::mint = share_mint, token::authority = burn_authority)]
    pub burn_shares: InterfaceAccount<'info, TokenAccountI>,
    pub share_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct BuybackStart<'info> {
    #[account(mut)]
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    #[account(init_if_needed, payer = rebalancer, space = 8 + Buyback::INIT_SPACE, seeds = [BUYBACK_SEED], bump)]
    pub buyback: Account<'info, Buyback>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mint::token_program = sell_token_program)]
    pub sell_mint: InterfaceAccount<'info, MintI>,
    #[account(mut, token::mint = sell_mint, token::authority = burn_authority, token::token_program = sell_token_program)]
    pub sell_source: InterfaceAccount<'info, TokenAccountI>,
    /// CHECK: receives the lent tokens; the token program validates it on transfer.
    #[account(mut)]
    pub rebalancer_sell: UncheckedAccount<'info>,
    #[account(seeds = [PRICE_SEED, sell_mint.key().as_ref()], bump = sell_price.bump)]
    pub sell_price: Account<'info, AssetPrice>,
    #[account(seeds = [PRICE_SEED, protocol.gravi_mint.as_ref()], bump = gravi_price.bump)]
    pub gravi_price: Account<'info, AssetPrice>,
    #[account(token::mint = protocol.gravi_mint, token::authority = burn_authority)]
    pub burn_gravi: InterfaceAccount<'info, TokenAccountI>,
    pub sell_token_program: Interface<'info, TokenInterface>,
    /// CHECK: the instructions sysvar, to find the matching finish.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct BuybackFinish<'info> {
    pub rebalancer: Signer<'info>,
    #[account(seeds = [KEEPERS_SEED], bump = keepers.bump, constraint = keepers.rebalancer == rebalancer.key() @ VaultError::NotRebalancer)]
    pub keepers: Account<'info, Keepers>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump)]
    pub protocol: Account<'info, Protocol>,
    #[account(mut, seeds = [BUYBACK_SEED], bump = buyback.bump)]
    pub buyback: Account<'info, Buyback>,
    /// CHECK: address-only PDA; signs as the owner of burn-earmarked accounts.
    #[account(seeds = [BURN_SEED], bump)]
    pub burn_authority: UncheckedAccount<'info>,
    #[account(mut, address = protocol.gravi_mint, mint::token_program = gravi_token_program)]
    pub gravi_mint: InterfaceAccount<'info, MintI>,
    #[account(mut, token::mint = gravi_mint, token::authority = burn_authority, token::token_program = gravi_token_program)]
    pub burn_gravi: InterfaceAccount<'info, TokenAccountI>,
    pub gravi_token_program: Interface<'info, TokenInterface>,
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
