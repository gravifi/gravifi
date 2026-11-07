//! GraviFi vault program: index baskets of Solana tokens, each a vault that holds its tokens
//! and issues one share token for them.
//!
//! This file is the index of instructions; each one's code and accounts live in
//! `instructions/`, grouped by what they do. `state.rs` holds the accounts and limits,
//! `token_util.rs` the token plumbing shared by both token programs.
use anchor_lang::prelude::*;

pub mod errors;
pub mod extensions;
pub mod instructions;
pub mod metaplex;
pub mod pyth;
pub mod state;
pub mod token_util;
pub mod valuation;

use instructions::*;
use state::*;

declare_id!("HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G");

#[program]
pub mod gravifi_vault {
    use super::*;

    // ------------------------------------------------------------------------------ protocol

    /// One-time protocol setup, gated on the program's upgrade authority. The Protocol
    /// account is a singleton, so without that gate anyone could initialise it first and
    /// appoint themselves the authority that decides which assets exist.
    pub fn init_protocol(
        ctx: Context<InitProtocol>,
        gravi_mint: Pubkey,
        protocol_fee_bps: u16,
        burn_share_bps: u16,
    ) -> Result<()> {
        instructions::protocol::init_protocol(ctx, gravi_mint, protocol_fee_bps, burn_share_bps)
    }

    /// Hands the protocol to a new authority, e.g. from the deployer to the team multisig.
    pub fn set_protocol_authority(ctx: Context<ProtocolAdmin>, new_authority: Pubkey) -> Result<()> {
        instructions::protocol::set_protocol_authority(ctx, new_authority)
    }

    /// Sets the GRAVI mint, once. The token launches after the program, so `init_protocol`
    /// may leave it unset; after this it can never change.
    pub fn set_gravi_mint(ctx: Context<ProtocolAdmin>, gravi_mint: Pubkey) -> Result<()> {
        instructions::protocol::set_gravi_mint(ctx, gravi_mint)
    }

    /// `Pubkey::default()` removes the guardian.
    pub fn set_guardian(ctx: Context<ProtocolAdmin>, guardian: Pubkey) -> Result<()> {
        instructions::protocol::set_guardian(ctx, guardian)
    }

    /// Names the lister, the one other key that may list assets (`register_asset`);
    /// `Pubkey::default()` removes it. Protocol authority only.
    pub fn set_lister(ctx: Context<SetLister>, lister: Pubkey) -> Result<()> {
        instructions::protocol::set_lister(ctx, lister)
    }

    pub fn set_protocol_fees(
        ctx: Context<ProtocolAdmin>,
        protocol_fee_bps: u16,
        burn_share_bps: u16,
    ) -> Result<()> {
        instructions::protocol::set_protocol_fees(ctx, protocol_fee_bps, burn_share_bps)
    }

    /// Sets the GRAVI a creator burns per asset in a basket, in base units; 0 turns it off.
    /// Capped at `MAX_BURN_PER_ASSET_TOKENS` whole GRAVI. Protocol authority only.
    pub fn set_creation_burn(ctx: Context<SetCreationBurn>, per_asset: u64) -> Result<()> {
        instructions::protocol::set_creation_burn(ctx, per_asset)
    }

    /// Adds a mint to the set of assets baskets may be built from: signed by the protocol
    /// authority, or by the lister with its account as the one remaining account.
    pub fn register_asset<'info>(ctx: Context<'_, '_, 'info, 'info, RegisterAsset<'info>>) -> Result<()> {
        instructions::protocol::register_asset(ctx)
    }

    /// Relists or delists a registered asset; delisting pauses minting into the baskets passed
    /// with it.
    pub fn set_asset_status<'info>(
        ctx: Context<'_, '_, 'info, 'info, SetAssetStatus<'info>>,
        status: AssetStatus,
    ) -> Result<()> {
        instructions::protocol::set_asset_status(ctx, status)
    }

    /// Moves protocol-treasury shares. Burn-earmarked shares have no equivalent: the burn
    /// PDA only signs inside the buyback instructions, so they can only ever be redeemed
    /// into the burn PDA's own accounts and sold for GRAVI that is burned.
    pub fn treasury_withdraw(ctx: Context<TreasuryWithdraw>, amount: u64) -> Result<()> {
        instructions::protocol::treasury_withdraw(ctx, amount)
    }

    // ------------------------------------------------------------------------------- baskets

    /// Creates the basket and its share mint, burning the creator's GRAVI for each of its assets
    /// if the protocol asks for it (`set_creation_burn`). Remaining accounts are one `[mint, asset_entry]` pair per
    /// constituent, in the order of `args.mints`.
    pub fn initialize_basket<'info>(
        ctx: Context<'_, '_, 'info, 'info, InitializeBasket<'info>>,
        args: InitBasketArgs,
    ) -> Result<()> {
        instructions::basket::initialize_basket(ctx, args)
    }

    // ----------------------------------------------------------------------- mint and redeem

    /// First deposit. Fixes `units_per_share` for every sleeve and mints the opening shares.
    /// Callable once, by the creator, regardless of access mode.
    pub fn seed_basket<'info>(
        ctx: Context<'_, '_, 'info, 'info, SeedBasket<'info>>,
        shares: u64,
        units_per_share: Vec<u64>,
    ) -> Result<()> {
        instructions::mint_redeem::seed_basket(ctx, shares, units_per_share)
    }

    /// Deposit the exact underlying basket for up to `shares`, receive them less the
    /// protocol's mint fee. No oracle is consulted.
    pub fn mint_in_kind<'info>(
        ctx: Context<'_, '_, 'info, 'info, MintInKind<'info>>,
        shares: u64,
        min_shares: u64,
    ) -> Result<()> {
        instructions::mint_redeem::mint_in_kind(ctx, shares, min_shares)
    }

    /// Hand in `shares`: the protocol's redeem fee is kept in shares, the rest are burned
    /// and their underlying paid out pro-rata. Never gated — not by access mode, not
    /// by a delisting. A holder who cannot redeem holds a promise, not a claim.
    pub fn redeem_in_kind<'info>(
        ctx: Context<'_, '_, 'info, 'info, RedeemInKind<'info>>,
        shares: u64,
        forfeit_mask: u16,
    ) -> Result<()> {
        instructions::mint_redeem::redeem_in_kind(ctx, shares, forfeit_mask)
    }

    // -------------------------------------------------------------------------------- upkeep

    /// Streams the creator's fee as share dilution. (The protocol takes nothing from
    /// holders: its fee is charged on mint and redeem.)
    pub fn accrue_fees<'info>(ctx: Context<'_, '_, 'info, 'info, AccrueFees<'info>>) -> Result<()> {
        instructions::upkeep::accrue_fees(ctx)
    }

    /// Permissionless reconciliation of the units promise against what the vault holds.
    pub fn sync_units<'info>(
        ctx: Context<'_, '_, 'info, 'info, SyncUnits<'info>>,
    ) -> Result<()> {
        instructions::upkeep::sync_units(ctx)
    }

    /// Permissionless: pauses minting if any constituent is delisted, and unpauses once
    /// all of them are listed again. Remaining accounts are the constituents' asset
    /// entries, in order.
    pub fn sync_listing_status<'info>(
        ctx: Context<'_, '_, 'info, 'info, SyncListing<'info>>,
    ) -> Result<()> {
        instructions::upkeep::sync_listing_status(ctx)
    }

    // --------------------------------------------------------------------- managing a basket

    /// Replace the target weights. `Immutable` rejects unconditionally, which is the whole
    /// promise of that mode; a smart basket's targets are its strategist's (`smart_set_targets`).
    pub fn set_targets(ctx: Context<Reconfigure>, target_bps: Vec<u16>) -> Result<()> {
        instructions::basket::set_targets(ctx, target_bps)
    }

    /// Adds a listed asset to a creator-managed basket at a 0% target and no units: nothing is
    /// deposited or moved, but the asset's GRAVI burn is paid, as at creation. A `set_targets`
    /// (normally in the same transaction) gives it a weight, and rebalancing then buys it in,
    /// slice by slice, like any underweight sleeve.
    pub fn add_constituent(ctx: Context<AddConstituent>) -> Result<()> {
        instructions::basket::add_constituent(ctx)
    }

    /// Drops an asset the basket no longer holds: its target must be 0% and rebalancing must
    /// have sold it out (no units per share), so no holder's claim changes. Whatever is still in
    /// its vault, rounding dust or tokens sent to it since, becomes a token the basket holds but
    /// doesn't count: the keeper's sweep (`sweep_start`) swaps it into one of the basket's own
    /// assets, for the holders, as it does with airdrops.
    pub fn remove_constituent(ctx: Context<Reconfigure>, index: u8) -> Result<()> {
        instructions::basket::remove_constituent(ctx, index)
    }

    /// Replace the rebalancing cadence and/or the creator fee.
    pub fn set_policy(
        ctx: Context<Reconfigure>,
        rebalance_mode: RebalanceMode,
        interval_secs: i64,
        drift_bps: u16,
        mgmt_fee_bps: u16,
    ) -> Result<()> {
        instructions::basket::set_policy(ctx, rebalance_mode, interval_secs, drift_bps, mgmt_fee_bps)
    }

    /// Sets the creator's cut of every mint (at most `MAX_CREATOR_CUT_BPS`) at once: down at any
    /// time, or to anything before the first deposit. Passing a pending rise's notice cancels it.
    /// A cut stops the yearly fee. Creator only.
    pub fn set_creator_cut(ctx: Context<SetCreatorCut>, bps: u16) -> Result<()> {
        instructions::creator_cut::set_creator_cut(ctx, bps)
    }

    /// Asks for a higher cut on a basket with shares out, applying a day from now. Creator only.
    pub fn propose_cut_rise(ctx: Context<ProposeCutRise>, bps: u16) -> Result<()> {
        instructions::creator_cut::propose_cut_rise(ctx, bps)
    }

    /// Applies a rise whose day's notice has run out. Permissionless.
    pub fn apply_cut_rise(ctx: Context<ApplyCutRise>) -> Result<()> {
        instructions::creator_cut::apply_cut_rise(ctx)
    }

    /// Moves what the creator is owed from the treasury's shares to theirs. Permissionless.
    pub fn claim_creator_cut(ctx: Context<ClaimCreatorCut>) -> Result<()> {
        instructions::creator_cut::claim_creator_cut(ctx)
    }

    /// Replaces the basket's stop losses (each a percentage below a reference price, fixed or
    /// trailing). The creator of a creator-managed basket at any time; of an immutable one only
    /// before its first deposit. Once seeded, no reference may be above its asset's fresh posted
    /// price (remaining accounts: each stop's AssetPrice, in order).
    pub fn set_stops<'info>(ctx: Context<'_, '_, 'info, 'info, SetStops<'info>>, stops: Vec<StopArgs>) -> Result<()> {
        instructions::stops::set_stops(ctx, stops)
    }

    /// Closes a creator-managed basket's stop account, its rent back to the creator.
    pub fn close_stops(ctx: Context<CloseStops>) -> Result<()> {
        instructions::stops::close_stops(ctx)
    }

    /// Checks one asset's stop against its posted price: arms it below the stop, fires it if still
    /// below STOP_CONFIRM_SECS to STOP_ARM_WINDOW_SECS later (its target to 0%, the others scaled
    /// up, within the guardrails on a smart or vote-run basket), disarms it above. Permissionless.
    pub fn check_stop<'info>(ctx: Context<'_, '_, 'info, 'info, CheckStop<'info>>) -> Result<()> {
        instructions::stops::check_stop(ctx)
    }

    /// Adds a wallet to an allowlisted basket. Creator-only.
    pub fn allow_wallet(ctx: Context<AllowWallet>) -> Result<()> {
        instructions::basket::allow_wallet(ctx)
    }

    /// Takes a wallet off an allowlisted basket (its approval closes, rent to the creator).
    /// Creator-only.
    pub fn disallow_wallet(ctx: Context<DisallowWallet>) -> Result<()> {
        instructions::basket::disallow_wallet(ctx)
    }

    // -------------------------------------------------------------------- keepers and prices

    /// Names who may post prices and who may run rebalances. Protocol authority only.
    pub fn init_keepers(ctx: Context<InitKeepers>, price_authority: Pubkey, rebalancer: Pubkey) -> Result<()> {
        instructions::protocol::init_keepers(ctx, price_authority, rebalancer)
    }

    pub fn set_keepers(ctx: Context<SetKeepers>, price_authority: Pubkey, rebalancer: Pubkey) -> Result<()> {
        instructions::protocol::set_keepers(ctx, price_authority, rebalancer)
    }

    /// Posts an asset's USD price (per whole token, x1e12). Bounded by its guard: within Pyth's
    /// confidence of a fresh Pyth price where the asset has a feed, else within 20% of an hourly
    /// reference.
    pub fn set_price(ctx: Context<SetPrice>, price_e12: u64) -> Result<()> {
        instructions::protocol::set_price(ctx, price_e12)
    }

    /// Names the Pyth feed an asset's prices are checked against (all zeros for none).
    /// Protocol authority only.
    pub fn set_price_feed(ctx: Context<SetPriceFeed>, feed_id: [u8; 32]) -> Result<()> {
        instructions::protocol::set_price_feed(ctx, feed_id)
    }

    /// Names the one wallet whose baskets burn no GRAVI (the team's); `Pubkey::default()`
    /// removes it. Protocol authority only.
    pub fn set_burn_exempt(ctx: Context<SetBurnExempt>, wallet: Pubkey) -> Result<()> {
        instructions::protocol::set_burn_exempt(ctx, wallet)
    }

    /// Gives a referred wallet half the protocol fee, all to the treasury, until `until` (at
    /// most a month and a day ahead). Once per wallet while the record exists. The wallet signs
    /// and pays the rent; the rebalancer co-signs.
    pub fn set_invite(ctx: Context<SetInvite>, until: i64) -> Result<()> {
        instructions::invite::set_invite(ctx, until)
    }

    /// Closes an invite that has run out, its rent back to the wallet. Permissionless.
    pub fn close_invite(ctx: Context<CloseInvite>) -> Result<()> {
        instructions::invite::close_invite(ctx)
    }

    // --------------------------------------------------------------------------- rebalancing

    /// First half of one rebalance slice: lends `sell_amount` of an overweight sleeve to
    /// the rebalancer, who swaps it on the market in the same transaction. The matching
    /// `rebalance_finish` must follow in that transaction and checks the vault got back at
    /// least the oracle value minus `max_slippage_bps` of the underweight sleeve. `refund` more
    /// of the sold token goes to the rebalancer for what the slice cost it, worth at most
    /// `MAX_KEEPER_REFUND_E12`.
    pub fn rebalance_start<'info>(
        ctx: Context<'_, '_, 'info, 'info, RebalanceStart<'info>>,
        sell_index: u8,
        buy_index: u8,
        sell_amount: u64,
        max_slippage_bps: u16,
        refund: u64,
    ) -> Result<()> {
        instructions::rebalance::rebalance_start(ctx, sell_index, buy_index, sell_amount, max_slippage_bps, refund)
    }

    /// A slice through SOL: sells an overweight sleeve into the basket's own wrapped SOL sleeve
    /// (`hub_index`), by at most what the sleeves under target still need, for a pair no single
    /// route fits a transaction; the next ordinary slice sells the SOL into them. As
    /// `rebalance_start` otherwise, `rebalance_finish` following.
    pub fn rebalance_start_via<'info>(
        ctx: Context<'_, '_, 'info, 'info, RebalanceStart<'info>>,
        sell_index: u8,
        hub_index: u8,
        sell_amount: u64,
        max_slippage_bps: u16,
        refund: u64,
    ) -> Result<()> {
        instructions::rebalance::rebalance_start_via(ctx, sell_index, hub_index, sell_amount, max_slippage_bps, refund)
    }

    /// Second half of a slice: checks the oracle floor, re-derives units for the two
    /// sleeves that traded, and ends the session once every sleeve is near target.
    pub fn rebalance_finish<'info>(ctx: Context<'_, '_, 'info, 'info, RebalanceFinish<'info>>) -> Result<()> {
        instructions::rebalance::rebalance_finish(ctx)
    }

    /// Swaps a token the basket was sent but doesn't hold (holder rewards, airdrops) into one
    /// of its own, against the oracle; `rebalance_finish` must follow in the same transaction.
    /// `refund`: as in `rebalance_start`, in the stray token.
    pub fn sweep_start<'info>(
        ctx: Context<'_, '_, 'info, 'info, SweepStart<'info>>,
        buy_index: u8,
        sell_amount: u64,
        max_slippage_bps: u16,
        refund: u64,
    ) -> Result<()> {
        instructions::rebalance::sweep_start(ctx, buy_index, sell_amount, max_slippage_bps, refund)
    }

    // ---------------------------------------------------------------------- buyback and burn

    /// Turns burn-earmarked shares of one basket into its underlying tokens, held by the
    /// burn PDA, ready to be sold for GRAVI. No fee, no price: exactly what a redeem pays.
    /// Permissionless, since value only moves from the basket to the burn PDA.
    pub fn buyback_redeem<'info>(
        ctx: Context<'_, '_, 'info, 'info, BuybackRedeem<'info>>,
        shares: u64,
    ) -> Result<()> {
        instructions::buyback::buyback_redeem(ctx, shares)
    }

    /// First half of a buyback slice: lends `sell_amount` of a token the burn PDA holds to
    /// the rebalancer, who swaps it for GRAVI in the same transaction. The matching
    /// `buyback_finish` must follow and checks at least the oracle value, less
    /// `max_slippage_bps`, came back as GRAVI. `refund`: up to $0.50 more of the sold token
    /// (MAX_BUYBACK_REFUND_E12), from the burn's budget, for the run's network costs.
    pub fn buyback_start(ctx: Context<BuybackStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
        instructions::buyback::buyback_start(ctx, sell_amount, max_slippage_bps, refund)
    }

    /// Second half of a buyback slice: checks the GRAVI that came back clears the oracle
    /// floor, then burns every GRAVI the burn PDA holds.
    pub fn buyback_finish(ctx: Context<BuybackFinish>) -> Result<()> {
        instructions::buyback::buyback_finish(ctx)
    }

    /// First half of a sale to SOL: lends `sell_amount` of a token the burn PDA holds to the
    /// rebalancer, who swaps it for wrapped SOL delivered to the burn PDA's own SOL account in
    /// the same transaction; `buyback_sol_finish` must follow. The SOL then buys GRAVI in one go
    /// (`buyback_start` selling SOL). `refund`: as in `buyback_start`.
    pub fn buyback_sol_start(ctx: Context<BuybackSolStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
        instructions::buyback::buyback_sol_start(ctx, sell_amount, max_slippage_bps, refund)
    }

    /// Second half of a sale to SOL: checks the SOL that came back clears the oracle floor.
    pub fn buyback_sol_finish(ctx: Context<BuybackSolFinish>) -> Result<()> {
        instructions::buyback::buyback_sol_finish(ctx)
    }

    /// Closes an empty token account the burn PDA owns and gives its rent back to the
    /// rebalancer, who paid it. Never the burn PDA's GRAVI account, never a share mint's.
    pub fn buyback_close(ctx: Context<BuybackClose>) -> Result<()> {
        instructions::buyback::buyback_close(ctx)
    }

    // ------------------------------------------------------------------------- smart baskets

    /// Names the strategist, the app's key that runs smart baskets within their guardrails
    /// (`Pubkey::default()` removes it), and sets the protocol fee on smart baskets' mints and
    /// redeems, at most `MAX_SMART_FEE_BPS` (`SMART_FEE_DEFAULT_BPS` until set). Protocol
    /// authority only.
    pub fn set_smart_config(ctx: Context<SetSmartConfig>, strategist: Pubkey, fee_bps: u16) -> Result<()> {
        instructions::smart::set_smart_config(ctx, strategist, fee_bps)
    }

    /// Sets the GRAVI a creator burns once on making a basket smart (`enable_smart`), on top of
    /// the per-asset creation burn: 0 turns it off, at most `MAX_SMART_BURN_TOKENS` whole GRAVI.
    /// Protocol authority only.
    pub fn set_smart_burn(ctx: Context<SetSmartBurn>, amount: u64) -> Result<()> {
        instructions::smart::set_smart_burn(ctx, amount)
    }

    /// Marks an asset GraviFi Verified, or takes the mark away. A smart basket holds an asset
    /// without it to at most its Unverified cap. Protocol authority only.
    pub fn set_verified(ctx: Context<SetVerified>, verified: bool) -> Result<()> {
        instructions::smart::set_verified(ctx, verified)
    }

    /// Makes a creator-managed basket smart, before its first deposit, with its mandate's hash
    /// and guardrails, burning the smart burn (`set_smart_burn`) from the creator's GRAVI.
    /// Creator only.
    pub fn enable_smart<'info>(ctx: Context<'_, '_, 'info, 'info, EnableSmart<'info>>, mandate_hash: [u8; 32], guard: Guardrails) -> Result<()> {
        instructions::smart::enable_smart(ctx, mandate_hash, guard)
    }

    /// Proposes a new mandate and guardrails, or handing the basket back to creator control
    /// (`leave`); it applies after a day's notice. An all-zero hash withdraws the waiting one.
    /// Creator only.
    pub fn propose_mandate(ctx: Context<CreatorSmart>, mandate_hash: [u8; 32], guard: Guardrails, leave: bool) -> Result<()> {
        instructions::smart::propose_mandate(ctx, mandate_hash, guard, leave)
    }

    /// Applies the waiting mandate once its notice is over. Permissionless.
    pub fn apply_mandate(ctx: Context<ApplyMandate>) -> Result<()> {
        instructions::smart::apply_mandate(ctx)
    }

    /// Pauses (or resumes) the strategist on a smart basket, which stays on its last targets.
    /// The creator or the guardian.
    pub fn set_smart_pause(ctx: Context<SetSmartPause>, paused: bool) -> Result<()> {
        instructions::smart::set_smart_pause(ctx, paused)
    }

    /// The strategist sets a smart basket's targets, within its guardrails. Remaining accounts:
    /// each constituent's Verified record address, in order.
    pub fn smart_set_targets<'info>(ctx: Context<'_, '_, 'info, 'info, StrategistBasket<'info>>, target_bps: Vec<u16>) -> Result<()> {
        instructions::smart::smart_set_targets(ctx, target_bps)
    }

    /// The strategist adds a listed asset to a smart basket at 0%. No GRAVI is burned.
    pub fn smart_add_constituent(ctx: Context<StrategistAdd>) -> Result<()> {
        instructions::smart::smart_add_constituent(ctx)
    }

    /// The strategist drops an asset a smart basket has sold out.
    pub fn smart_remove_constituent(ctx: Context<StrategistBasket>, index: u8) -> Result<()> {
        instructions::smart::smart_remove_constituent(ctx, index)
    }

    /// The strategist puts the mandate's stop on an asset of a smart basket that has none.
    pub fn smart_set_stop(ctx: Context<StrategistStop>) -> Result<()> {
        instructions::smart::smart_set_stop(ctx)
    }

    // ------------------------------------------------------------------------- share token

    /// Names the share token in wallets: creates its Metaplex metadata the first time (`payer`
    /// pays Metaplex's fee and the rent), replaces its name, symbol and URI after. The
    /// basket's creator or the protocol authority; display only, so immutable baskets too.
    pub fn set_share_metadata(ctx: Context<SetShareMetadata>, name: String, symbol: String, uri: String) -> Result<()> {
        instructions::metadata::set_share_metadata(ctx, name, symbol, uri)
    }

    // --------------------------------------------------------------------- legacy and splits

    /// Holder governance was removed. A basket that had been handed to its holders is
    /// otherwise frozen (no governor can act for it any more), so the protocol authority can
    /// return it to its creator. Only ever acts on such a basket, and none can be created now.
    pub fn restore_creator_control(ctx: Context<RestoreControl>) -> Result<()> {
        instructions::basket::restore_creator_control(ctx)
    }

    /// Splits (or reverse-splits) the basket's shares by `numerator`:`denominator` by
    /// changing the share mint's display multiplier. Balances shown in wallets scale by the
    /// ratio and NAV per share by its inverse; no tokens move and nothing needs minting.
    /// Only the creator of a creator-managed basket can split it.
    pub fn split(ctx: Context<Split>, numerator: u16, denominator: u16) -> Result<()> {
        instructions::basket::split(ctx, numerator, denominator)
    }

    // ------------------------------------------------------------------------- GRAVI staking

    /// Sets up the stake pool and turns staking on, for good. Protocol authority, once.
    pub fn init_staking(ctx: Context<InitStaking>) -> Result<()> {
        instructions::staking::init_staking(ctx)
    }

    /// Moves the staking threshold, up to its ceiling; a raise applies after a week's notice.
    pub fn set_stake_threshold(ctx: Context<SetStakeThreshold>, threshold: u64) -> Result<()> {
        instructions::staking::set_stake_threshold(ctx, threshold)
    }

    /// Stakes GRAVI. Perks start two days after the stake reaches the threshold.
    pub fn stake(ctx: Context<StakeMove>, amount: u64) -> Result<()> {
        instructions::staking::stake(ctx, amount)
    }

    /// Starts staked GRAVI on its seven days out. It earns and skips nothing meanwhile.
    pub fn unstake(ctx: Context<StakeUpdate>, amount: u64) -> Result<()> {
        instructions::staking::unstake(ctx, amount)
    }

    /// Pays out GRAVI that has waited out its days.
    pub fn stake_withdraw(ctx: Context<StakeWithdraw>) -> Result<()> {
        instructions::staking::stake_withdraw(ctx)
    }

    /// Pays a stake the SOL it has earned, as wrapped SOL.
    pub fn stake_claim(ctx: Context<StakeClaim>) -> Result<()> {
        instructions::staking::stake_claim(ctx)
    }

    /// Closes an empty, paid-out stake, its rent back to the wallet.
    pub fn stake_close(ctx: Context<StakeClose>) -> Result<()> {
        instructions::staking::stake_close(ctx)
    }

    /// Brings a stake in line with the clock and the threshold. Permissionless.
    pub fn stake_refresh(ctx: Context<StakeRefresh>) -> Result<()> {
        instructions::staking::stake_refresh(ctx)
    }

    /// Starts the next month's pot once the last has paid out. Permissionless.
    pub fn stake_fund(ctx: Context<StakeFund>) -> Result<()> {
        instructions::staking::stake_fund(ctx)
    }

    /// Moves the shares a basket owes stakers from its treasury to the rewards PDA. Permissionless.
    pub fn stake_sweep(ctx: Context<StakeSweep>) -> Result<()> {
        instructions::staking::stake_sweep(ctx)
    }

    /// Redeems swept shares into tokens the rewards PDA holds. Permissionless.
    pub fn stake_redeem<'info>(ctx: Context<'_, '_, 'info, 'info, StakeRedeem<'info>>, shares: u64) -> Result<()> {
        instructions::staking::stake_redeem(ctx, shares)
    }

    /// First half of a sale to SOL for the pot. Rebalancer only.
    pub fn stake_sell_start(ctx: Context<StakeSellStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
        instructions::staking::stake_sell_start(ctx, sell_amount, max_slippage_bps, refund)
    }

    /// Second half of a sale to SOL: checks the oracle floor. Rebalancer only.
    pub fn stake_sell_finish(ctx: Context<StakeSellFinish>) -> Result<()> {
        instructions::staking::stake_sell_finish(ctx)
    }

    /// Closes an empty token account the rewards PDA owns. Rebalancer only.
    pub fn stake_close_account(ctx: Context<StakeCloseAccount>) -> Result<()> {
        instructions::staking::stake_close_account(ctx)
    }

    // ------------------------------------------------------------- share staking, community

    /// Opens or closes the door to Community mode. Protocol authority.
    pub fn set_community_open(ctx: Context<SetCommunityOpen>, open: bool) -> Result<()> {
        instructions::community::set_community_open(ctx, open)
    }

    /// Stakes a basket's shares. Any basket; votes count only in community ones.
    pub fn share_stake(ctx: Context<ShareStakeIn>, amount: u64) -> Result<()> {
        instructions::community::share_stake(ctx, amount)
    }

    /// Takes staked shares back, unless they voted in a vote still running.
    pub fn share_unstake(ctx: Context<ShareStakeOut>, amount: u64) -> Result<()> {
        instructions::community::share_unstake(ctx, amount)
    }

    /// Hands a seeded, public, creator-managed basket to its holders. Creator only.
    pub fn enter_community(ctx: Context<EnterCommunity>) -> Result<()> {
        instructions::community::enter_community(ctx)
    }

    /// Opens a proposal; needs 1% of the supply in staked weight.
    pub fn community_propose<'info>(ctx: Context<'_, '_, 'info, 'info, CommunityPropose<'info>>, action: ProposalAction) -> Result<()> {
        instructions::community::community_propose(ctx, action)
    }

    /// Votes a wallet's weight as of the proposal's opening.
    pub fn community_vote(ctx: Context<CommunityVote>, support: bool) -> Result<()> {
        instructions::community::community_vote(ctx, support)
    }

    /// Runs a passed Reweight, Policy, CreatorFee or Leave. Permissionless.
    pub fn community_execute<'info>(ctx: Context<'_, '_, 'info, 'info, CommunityExecute<'info>>) -> Result<()> {
        instructions::community::community_execute(ctx)
    }

    /// Runs a passed AddAsset. Permissionless.
    pub fn community_execute_add(ctx: Context<CommunityExecuteAdd>) -> Result<()> {
        instructions::community::community_execute_add(ctx)
    }

    /// Runs a passed Split. Permissionless.
    pub fn community_execute_split(ctx: Context<CommunityExecuteSplit>) -> Result<()> {
        instructions::community::community_execute_split(ctx)
    }

    /// Runs a passed Stops. Permissionless.
    pub fn community_execute_stops<'info>(ctx: Context<'_, '_, 'info, 'info, CommunityExecuteStops<'info>>) -> Result<()> {
        instructions::community::community_execute_stops(ctx)
    }

    /// Runs a passed GoSmart, Mandate, or a smart basket's Leave. Permissionless.
    pub fn community_execute_smart(ctx: Context<CommunityExecuteSmart>) -> Result<()> {
        instructions::community::community_execute_smart(ctx)
    }

    /// Drops an asset a community basket has sold out. Permissionless.
    pub fn community_remove(ctx: Context<CommunityRemove>, index: u8) -> Result<()> {
        instructions::community::community_remove(ctx, index)
    }

    /// Closes a finished proposal, its rent back to the proposer. Permissionless.
    pub fn community_close(ctx: Context<CommunityClose>) -> Result<()> {
        instructions::community::community_close(ctx)
    }

    /// Closes a vote record once its vote has ended. Permissionless.
    pub fn community_close_vote(ctx: Context<CommunityCloseVote>) -> Result<()> {
        instructions::community::community_close_vote(ctx)
    }
}
