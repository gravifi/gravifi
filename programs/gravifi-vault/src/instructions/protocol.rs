//! Protocol administration: the protocol's settings and fees, which assets are listed, who
//! the keepers are, oracle prices, and the treasury.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint as MintI, TokenAccount as TokenAccountI, TokenInterface};

use crate::errors::VaultError;
use crate::extensions;
use crate::params::{self, Params, PARAMS_ID};
use crate::pyth;
use crate::state::*;
use crate::token_util::*;

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
    check_burn_share(burn_share_bps)?;

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

// --------------------------------------------------------------------------- set_role

pub const ROLE_AUTHORITY: u8 = 0;
pub const ROLE_GUARDIAN: u8 = 1;
/// Once: the token launches after the program, so `init_protocol` may leave it unset.
pub const ROLE_GRAVI_MINT: u8 = 2;
pub const ROLE_BURN_EXEMPT: u8 = 3;
pub const ROLE_LISTER: u8 = 4;
pub const ROLE_STRATEGIST: u8 = 5;
pub const ROLE_PRICE_AUTHORITY: u8 = 6;
pub const ROLE_REBALANCER: u8 = 7;

/// Names the key for a role; `Pubkey::default()` removes it (not the authority, nor the GRAVI
/// mint, which is set once). Protocol authority only.
///
/// - authority: hands the protocol on, e.g. to the team's multisig.
/// - guardian: may delist and pause smart baskets, never list.
/// - GRAVI mint: set once, never changed.
/// - burn-exempt: the one wallet whose baskets burn no GRAVI (the team's); it sits after the
///   protocol's fields, which the account grows to hold the first time.
/// - lister, strategist: their accounts (`[LISTER_SEED]`, `[STRATEGIST_SEED]`) are the one remaining
///   account, created the first time.
/// - price authority, rebalancer: the keepers' account (`[KEEPERS_SEED]`) is the one remaining
///   account, created the first time with the other key unset.
pub fn set_role<'info>(ctx: Context<'_, '_, 'info, 'info, ProtocolConfig<'info>>, role: u8, key: Pubkey) -> Result<()> {
    let a = &mut *ctx.accounts;
    let (payer, system) = (a.authority.to_account_info(), a.system_program.to_account_info());
    match role {
        ROLE_AUTHORITY => {
            require!(key != Pubkey::default(), VaultError::BadParameter);
            a.protocol.authority = key;
        }
        ROLE_GUARDIAN => a.protocol.guardian = key,
        ROLE_GRAVI_MINT => {
            require!(a.protocol.gravi_mint == Pubkey::default(), VaultError::GraviMintAlreadySet);
            require!(key != Pubkey::default(), VaultError::BadParameter);
            a.protocol.gravi_mint = key;
        }
        ROLE_BURN_EXEMPT => {
            let ai = a.protocol.to_account_info();
            grow(&ai, &payer, &system, BURN_EXEMPT_OFFSET + 32)?;
            ai.try_borrow_mut_data()?[BURN_EXEMPT_OFFSET..BURN_EXEMPT_OFFSET + 32].copy_from_slice(key.as_ref());
        }
        ROLE_LISTER | ROLE_STRATEGIST => {
            let seed = if role == ROLE_LISTER { LISTER_SEED } else { STRATEGIST_SEED };
            let (ai, bump) = role_account(ctx.remaining_accounts, seed, 8 + Lister::INIT_SPACE, &payer, &system)?;
            // Lister and Strategist are the same shape: [discriminator][bump][key].
            let disc: &[u8] = if role == ROLE_LISTER { Lister::DISCRIMINATOR } else { Strategist::DISCRIMINATOR };
            let mut d = ai.try_borrow_mut_data()?;
            d[..8].copy_from_slice(disc);
            d[8] = bump;
            d[9..41].copy_from_slice(key.as_ref());
        }
        ROLE_PRICE_AUTHORITY | ROLE_REBALANCER => {
            let (ai, bump) = role_account(ctx.remaining_accounts, KEEPERS_SEED, 8 + Keepers::INIT_SPACE, &payer, &system)?;
            let mut d = ai.try_borrow_mut_data()?;
            d[..8].copy_from_slice(Keepers::DISCRIMINATOR);
            d[8] = bump;
            let at = if role == ROLE_PRICE_AUTHORITY { 9 } else { 41 };
            d[at..at + 32].copy_from_slice(key.as_ref());
        }
        _ => return err!(VaultError::BadRole),
    }
    Ok(())
}

/// The role's account `[seed]` and its bump: the one remaining account, created (`space` bytes) the
/// first time.
fn role_account<'a, 'info>(remaining: &'a [AccountInfo<'info>], seed: &[u8], space: usize, payer: &AccountInfo<'info>, system: &AccountInfo<'info>) -> Result<(&'a AccountInfo<'info>, u8)> {
    let ai = remaining.first().ok_or(VaultError::BadRole)?;
    let (expected, bump) = Pubkey::find_program_address(&[seed], &crate::ID);
    require_keys_eq!(*ai.key, expected, VaultError::BadRole);
    if ai.owner != &crate::ID {
        create_pda(payer, ai, system, &[seed, &[bump]], space)?;
    }
    Ok((ai, bump))
}

// -------------------------------------------------------------------------- set_param

/// The protocol fee and the burn's share of it: `fee_bps | burn_share_bps << 16`. At once, within
/// `MAX_PROTOCOL_FEE_BPS` and from `MIN_BURN_SHARE_BPS`, both code.
pub const CONFIG_PROTOCOL_FEES: u16 = 1_000;
/// The GRAVI a creator burns per asset, in base units, up to `MAX_BURN_PER_ASSET_TOKENS` whole
/// GRAVI; 0 turns it off. Remaining accounts: the creation burn's account (`[CREATION_BURN_SEED]`,
/// created the first time) and the GRAVI mint.
pub const CONFIG_CREATION_BURN: u16 = 1_001;
/// The protocol fee on smart baskets, up to `MAX_SMART_FEE_BPS`.
pub const CONFIG_SMART_FEE: u16 = 1_002;
/// The GRAVI burned once on making a basket smart, up to `MAX_SMART_BURN_TOKENS` whole GRAVI.
/// Remaining accounts: the GRAVI mint.
pub const CONFIG_SMART_BURN: u16 = 1_003;
/// The staking threshold, in GRAVI base units, up to `MAX_STAKE_THRESHOLD_TOKENS`: lower at once,
/// higher after `STAKE_THRESHOLD_NOTICE_SECS`. Remaining accounts: the stake pool.
pub const CONFIG_STAKE_THRESHOLD: u16 = 1_004;
/// 1 opens the door to Community mode, 0 closes it (baskets already in carry on).
pub const CONFIG_COMMUNITY_OPEN: u16 = 1_005;

/// Sets one of the protocol's tunables (`params.rs`: ids below 1,000, each within the bounds and
/// under the notice written there; 0 returns it to its default), or one of its settings kept
/// elsewhere (`CONFIG_*`, at once as before). The params account is created the first time, the
/// authority paying its rent. Protocol authority only.
pub fn set_param<'info>(ctx: Context<'_, '_, 'info, 'info, ProtocolConfig<'info>>, id: u16, value: u64) -> Result<()> {
    let a = &mut *ctx.accounts;
    let (payer, system) = (a.authority.to_account_info(), a.system_program.to_account_info());
    if id < 1_000 {
        let ai = a.params.to_account_info();
        params::ensure(&ai, &payer, &system)?;
        let now = Clock::get()?.unix_timestamp;
        return params::write(&mut ai.try_borrow_mut_data()?, id, value, now);
    }
    let protocol = a.protocol.to_account_info();
    match id {
        CONFIG_PROTOCOL_FEES => {
            let (fee, burn_share) = ((value & 0xffff) as u16, ((value >> 16) & 0xffff) as u16);
            require!(value >> 32 == 0 && fee <= MAX_PROTOCOL_FEE_BPS, VaultError::FeeTooHigh);
            check_burn_share(burn_share)?;
            a.protocol.protocol_fee_bps = fee;
            a.protocol.burn_share_bps = burn_share;
        }
        CONFIG_CREATION_BURN => {
            let [cb, mint] = ctx.remaining_accounts else { return err!(VaultError::BadRemainingAccounts) };
            require!(value <= whole_gravi(&a.protocol, mint, MAX_BURN_PER_ASSET_TOKENS)?, VaultError::CreationBurnTooHigh);
            let (expected, bump) = Pubkey::find_program_address(&[CREATION_BURN_SEED], &crate::ID);
            require_keys_eq!(*cb.key, expected, VaultError::BadParameter);
            let mut c = if cb.owner == &crate::ID {
                CreationBurn::try_deserialize(&mut &cb.try_borrow_data()?[..])?
            } else {
                create_pda(&payer, cb, &system, &[CREATION_BURN_SEED, &[bump]], 8 + CreationBurn::INIT_SPACE)?;
                CreationBurn { bump, per_asset: 0, total_burned: 0, baskets: 0 }
            };
            c.per_asset = value;
            c.try_serialize(&mut &mut cb.try_borrow_mut_data()?[..])?;
        }
        CONFIG_SMART_FEE => {
            require!(value <= MAX_SMART_FEE_BPS as u64, VaultError::FeeTooHigh);
            grow(&protocol, &payer, &system, SMART_FEE_OFFSET + SMART_FEE_LEN)?;
            let [lo, hi] = (value as u16).to_le_bytes();
            protocol.try_borrow_mut_data()?[SMART_FEE_OFFSET..SMART_FEE_OFFSET + SMART_FEE_LEN].copy_from_slice(&[lo, hi, 1]);
        }
        CONFIG_SMART_BURN => {
            let [mint] = ctx.remaining_accounts else { return err!(VaultError::BadRemainingAccounts) };
            require!(value <= whole_gravi(&a.protocol, mint, MAX_SMART_BURN_TOKENS)?, VaultError::CreationBurnTooHigh);
            grow(&protocol, &payer, &system, SMART_BURN_OFFSET + SMART_BURN_LEN)?;
            protocol.try_borrow_mut_data()?[SMART_BURN_OFFSET..SMART_BURN_OFFSET + SMART_BURN_LEN].copy_from_slice(&value.to_le_bytes());
        }
        CONFIG_STAKE_THRESHOLD => {
            let [pool_ai] = ctx.remaining_accounts else { return err!(VaultError::BadRemainingAccounts) };
            require_keys_eq!(*pool_ai.owner, crate::ID, VaultError::BadStake);
            let mut p = StakePool::try_deserialize(&mut &pool_ai.try_borrow_data()?[..])?;
            let expected = Pubkey::create_program_address(&[STAKE_POOL_SEED, &[p.bump]], &crate::ID).map_err(|_| VaultError::BadStake)?;
            require_keys_eq!(*pool_ai.key, expected, VaultError::BadStake);
            let now = Clock::get()?.unix_timestamp;
            let ceiling = MAX_STAKE_THRESHOLD_TOKENS.checked_mul(p.one_gravi).ok_or(VaultError::MathOverflow)?;
            require!(value > 0 && value <= ceiling, VaultError::BadStakeThreshold);
            p.threshold = p.threshold_at(now);
            p.next_threshold_at = 0;
            if value <= p.threshold {
                p.threshold = value;
            } else {
                p.next_threshold = value;
                p.next_threshold_at = now + STAKE_THRESHOLD_NOTICE_SECS;
            }
            p.try_serialize(&mut &mut pool_ai.try_borrow_mut_data()?[..])?;
        }
        CONFIG_COMMUNITY_OPEN => {
            require!(value <= 1, VaultError::BadParameter);
            set_protocol_flag(&protocol, &payer, &system, FLAG_COMMUNITY, value == 1)?;
        }
        _ => return err!(VaultError::UnknownParam),
    }
    Ok(())
}

/// `tokens` whole GRAVI in base units, read off the GRAVI mint (which must be the protocol's).
fn whole_gravi(protocol: &Protocol, mint: &AccountInfo, tokens: u64) -> Result<u64> {
    require!(protocol.gravi_mint != Pubkey::default(), VaultError::GraviMintNotSet);
    require_keys_eq!(*mint.key, protocol.gravi_mint, VaultError::GraviMintNotSet);
    10u64.checked_pow(mint_decimals(mint)? as u32).and_then(|one| one.checked_mul(tokens)).ok_or(VaultError::MathOverflow.into())
}

/// Sets a protocol flag, growing the protocol account's tail to hold it the first time.
pub(crate) fn set_protocol_flag<'info>(protocol: &AccountInfo<'info>, payer: &AccountInfo<'info>, system_program: &AccountInfo<'info>, flag: u8, on: bool) -> Result<()> {
    grow(protocol, payer, system_program, PROTOCOL_FLAGS_OFFSET + 1)?;
    let mut data = protocol.try_borrow_mut_data()?;
    data[PROTOCOL_FLAGS_OFFSET] = if on { data[PROTOCOL_FLAGS_OFFSET] | flag } else { data[PROTOCOL_FLAGS_OFFSET] & !flag };
    Ok(())
}

/// Adds a mint to the set of assets baskets may be built from. Signed by the protocol authority,
/// or by the lister (`set_role` ROLE_LISTER), whose account is then the one remaining account. (A remaining
/// account rather than a named one, so every client listing as the authority is unchanged.)
pub fn register_asset<'info>(ctx: Context<'_, '_, 'info, 'info, RegisterAsset<'info>>) -> Result<()> {
    let signer = ctx.accounts.authority.key();
    let is_authority = signer == ctx.accounts.protocol.authority;
    let by_lister = match ctx.remaining_accounts.first() {
        Some(ai) if !is_authority => {
            let (expected, _) = Pubkey::find_program_address(&[LISTER_SEED], &crate::ID);
            require_keys_eq!(*ai.key, expected, VaultError::NotProtocolAuthority);
            let l = Account::<Lister>::try_from(ai)?;
            l.lister != Pubkey::default() && l.lister == signer
        }
        _ => false,
    };
    require!(is_authority || by_lister, VaultError::NotProtocolAuthority);
    let prm = Params::load(&ctx.accounts.params)?;
    // The kill switch stops the lister; the authority, who sets it, may still list.
    if !is_authority {
        prm.require_on(params::FAMILY_LISTING)?;
    }
    assert_no_active_transfer_hook(&ctx.accounts.mint.to_account_info())?;
    // The lister lists only what a vault can hold as it comes. The authority may still list a
    // token whose new accounts start frozen, for an issuer that approves the vaults it holds.
    if !is_authority {
        assert_holdable(&ctx.accounts.mint.to_account_info())?;
    }
    TransferFee::of(&ctx.accounts.mint.to_account_info())?.assert_within_cap(prm.bps(params::TRANSFER_FEE_CAP_BPS))?;

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
/// Delisting pauses minting into the baskets that hold the asset in the same instruction: the
/// remaining accounts are those baskets, each writable. Pausing them here, not in a later
/// `sync_listing_status`, leaves no window in which new money can still go into a basket the
/// protocol has stopped vouching for. (A basket left out can still be paused by that sync.)
/// Relisting unpauses nothing: another of a basket's assets may still be delisted, so that's
/// left to `sync_listing_status`, which checks them all.
///
/// The guardian may delist but never list, and the asymmetry is the point. Listing is
/// the action worth bribing a vote for, so it must go through governance. Delisting is
/// the action that has to happen in minutes when a listed token turns out to be
/// compromised, so it cannot wait out a vote and a timelock.
pub fn set_asset_status<'info>(ctx: Context<'_, '_, 'info, 'info, SetAssetStatus<'info>>, status: AssetStatus) -> Result<()> {
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

    // Before the early return below, so a repeat delisting can still pause baskets missed.
    if status == AssetStatus::Delisted {
        let mint = ctx.accounts.mint.key();
        for ai in ctx.remaining_accounts {
            let mut basket = Account::<Basket>::try_from(ai)?;
            require!(basket.active().iter().any(|c| c.mint == mint), VaultError::ConstituentMismatch);
            basket.mint_paused = true;
            basket.exit(&crate::ID)?;
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
    let basket = &ctx.accounts.basket;
    require!(ctx.accounts.treasury_shares.amount.saturating_sub(basket.owed_to_creator()) >= amount, VaultError::TreasuryOwedToCreator);
    require!(ctx.accounts.treasury_shares.amount.saturating_sub(basket.owed_to_creator()).saturating_sub(basket.owed_to_stakers()) >= amount, VaultError::TreasuryOwedToStakers);
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

/// Posts an asset's USD price (per whole token, x1e12), which rebalance and buyback floors are
/// checked against. Bounded by the asset's guard: for an asset with a Pyth feed, within Pyth's
/// confidence (3-5%) of a fresh Pyth price; for any other, within 20% of a reference that rolls
/// forward hourly. Pyth replaces the band where there is one: it's the tighter bound, and
/// independent of the keeper, so an asset whose last price is far from the market (unposted
/// for days) can be posted at once instead of never fitting both.
pub fn set_price(ctx: Context<SetPrice>, price_e12: u64) -> Result<()> {
    require!(price_e12 > 0, VaultError::BadPrice);
    let prm = Params::load(&ctx.accounts.params)?;
    let now = prm.now;
    let mint_ai = ctx.accounts.mint.to_account_info();
    let decimals = mint_decimals(&mint_ai)?;
    let p = &mut ctx.accounts.asset_price;
    let g = &mut ctx.accounts.price_guard;
    g.bump = ctx.bumps.price_guard;
    g.mint = ctx.accounts.mint.key();
    // A guard created after prices were already posted starts from the last one.
    if g.reference_e12 == 0 && p.price_e12 > 0 {
        g.reference_e12 = p.price_e12;
        g.reference_at = p.updated_at;
    }
    if g.pyth_feed == [0u8; 32] {
        if g.reference_e12 > 0 {
            require!(within(price_e12 as u128, g.reference_e12 as u128, prm.wide(params::PRICE_MOVE_BPS)), VaultError::PriceMoveTooLarge);
        }
    } else {
        let ai = ctx.accounts.pyth_price.as_ref().ok_or(VaultError::PythPriceMissing)?;
        let pp = pyth::read(ai, &g.pyth_feed, now, prm.secs(params::PYTH_AGE_SECS))?;
        let multiplier = extensions::current_multiplier(&mint_ai.try_borrow_data()?, now).unwrap_or(1.0);
        let pyth_e12 = pp.e12(pp.price as u64, multiplier).ok_or(VaultError::PythPriceInvalid)?;
        let conf_bps = (pp.conf as u128 * BPS_DENOM) / pp.price as u128;
        let hi = prm.wide(params::PYTH_TOLERANCE_MAX_BPS);
        let tolerance = (conf_bps * prm.wide(params::PYTH_CONF_MULTIPLE)).clamp(prm.wide(params::PYTH_TOLERANCE_MIN_BPS).min(hi), hi);
        require!(pyth_e12 > 0 && within(price_e12 as u128, pyth_e12, tolerance), VaultError::PriceOffPyth);
    }
    if g.reference_e12 == 0 || now - g.reference_at >= prm.secs(params::PRICE_REFERENCE_SECS) {
        g.reference_e12 = price_e12;
        g.reference_at = now;
    }
    p.bump = ctx.bumps.asset_price;
    p.mint = ctx.accounts.mint.key();
    p.decimals = decimals;
    p.price_e12 = price_e12;
    p.updated_at = now;
    Ok(())
}

/// Whether `price` is within `bps` of `reference`, either way.
fn within(price: u128, reference: u128, bps: u128) -> bool {
    price.abs_diff(reference) * BPS_DENOM <= reference * bps
}

/// Names the Pyth feed an asset's posted prices are checked against; all zeros removes it.
/// Pyth's feeds name no mint, so each one is matched to its token by hand. Protocol authority.
pub fn set_price_feed(ctx: Context<SetPriceFeed>, feed_id: [u8; 32]) -> Result<()> {
    let g = &mut ctx.accounts.price_guard;
    g.bump = ctx.bumps.price_guard;
    g.mint = ctx.accounts.mint.key();
    g.pyth_feed = feed_id;
    Ok(())
}

/// Checks `expected` is the upgrade authority recorded in this program's ProgramData.
///
/// Parsed by hand rather than through `Account<ProgramData>`, which drags a bincode
/// deserialiser into the binary — tens of KB of rent to read one field. The layout is the
/// loader's fixed bincode encoding of `UpgradeableLoaderState::ProgramData`:
/// `u32` variant (3), `u64` slot, `u8` option tag, then the 32-byte authority.
/// The burn's share of the protocol fee: anywhere from `MIN_BURN_SHARE_BPS` (half) to all of it.
/// The floor is code, like the fee ceilings, so the half that buys GRAVI and burns it can't be
/// turned down by the authority's key.
pub(crate) fn check_burn_share(burn_share_bps: u16) -> Result<()> {
    require!(
        burn_share_bps >= MIN_BURN_SHARE_BPS && burn_share_bps as u128 <= BPS_DENOM,
        VaultError::BurnShareTooLow
    );
    Ok(())
}

pub(crate) fn assert_upgrade_authority(program_data: &AccountInfo, expected: &Pubkey) -> Result<()> {
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

// ---------------------------------------------------------------------------- accounts

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

/// The protocol authority's settings (`set_role`, `set_param`).
#[derive(Accounts)]
pub struct ProtocolConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// Grown by hand for the settings kept after its fields (burn-exempt, smart fee and burn, flags).
    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    /// CHECK: the params account (`params.rs`), created by hand the first time a value is set.
    #[account(mut, address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterAsset<'info> {
    /// Pays the entry's rent. Separate from `authority`, which is usually a multisig, or the
    /// lister, whose listings the creator asking for them pays for.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// The protocol authority, or the lister when `lister` is passed (checked in the handler).
    pub authority: Signer<'info>,

    #[account(mut, seeds = [PROTOCOL_SEED], bump = protocol.bump)]
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
    /// CHECK: the params account (`params.rs`), read for the listing switch and the fee cap.
    #[account(address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
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

    /// Read for what its treasury shares owe its creator and stakers.
    #[account(has_one = share_mint)]
    pub basket: Account<'info, Basket>,

    pub share_mint: InterfaceAccount<'info, MintI>,

    #[account(mut, token::mint = share_mint, token::authority = protocol, constraint = is_ata(&treasury_shares.to_account_info(), &protocol.key(), &share_mint.key()) @ VaultError::NotBasketVault)]
    pub treasury_shares: InterfaceAccount<'info, TokenAccountI>,

    #[account(mut, token::mint = share_mint)]
    pub destination: InterfaceAccount<'info, TokenAccountI>,

    pub share_token_program: Interface<'info, TokenInterface>,
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
    #[account(init_if_needed, payer = price_authority, space = 8 + PriceGuard::INIT_SPACE, seeds = [GUARD_SEED, mint.key().as_ref()], bump)]
    pub price_guard: Account<'info, PriceGuard>,
    /// CHECK: the asset's Pyth price update, read by `pyth::read` (owner, feed, verification,
    /// age). Needed only when its guard names a feed.
    pub pyth_price: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
    /// CHECK: the params account (`params.rs`), read for the price bounds.
    #[account(address = PARAMS_ID)]
    pub params: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetPriceFeed<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol.bump, has_one = authority @ VaultError::NotProtocolAuthority)]
    pub protocol: Account<'info, Protocol>,
    pub mint: InterfaceAccount<'info, MintI>,
    #[account(init_if_needed, payer = authority, space = 8 + PriceGuard::INIT_SPACE, seeds = [GUARD_SEED, mint.key().as_ref()], bump)]
    pub price_guard: Account<'info, PriceGuard>,
    pub system_program: Program<'info, System>,
}
