//! GraviFi vault program: index baskets of Solana tokens, each a vault that holds its tokens
//! and issues one share token for them.
//!
//! This file is the index of instructions; each one's code and accounts live in
//! `instructions/`, grouped by what they do. `state.rs` holds the accounts and limits,
//! `token_util.rs` the token plumbing shared by both token programs.
use anchor_lang::prelude::*;

pub mod errors;
pub mod ext;
pub mod extensions;
pub mod instructions;
pub mod metaplex;
pub mod params;
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

    /// Names the key for a role (`ROLE_*` in `instructions/protocol.rs`): the authority, the
    /// guardian, the GRAVI mint (once), the burn-exempt wallet, the lister, the strategist, the
    /// price authority or the rebalancer. A key named acts after a day; removing a role is at
    /// once. Protocol authority only.
    pub fn set_role<'info>(ctx: Context<'_, '_, 'info, 'info, ProtocolConfig<'info>>, role: u8, key: Pubkey) -> Result<()> {
        instructions::protocol::set_role(ctx, role, key)
    }

    /// Sets one of the protocol's tunables (`params.rs`), within the bounds and under the notice
    /// written in the code, or one of its settings kept elsewhere (`CONFIG_*`: fees, burns, the
    /// staking threshold, Community's door; a fee or burn raise waits a week, lowering is at
    /// once). Protocol authority only.
    pub fn set_param<'info>(ctx: Context<'_, '_, 'info, 'info, ProtocolConfig<'info>>, id: u16, value: u64) -> Result<()> {
        instructions::protocol::set_param(ctx, id, value)
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
    /// if the protocol asks for it (`set_param` CONFIG_CREATION_BURN). Remaining accounts are one `[mint, asset_entry]` pair per
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
    /// time, or to anything before the first deposit. It calls off a pending rise (passing the
    /// rise's notice closes it). A cut stops the yearly fee. Creator only.
    pub fn set_creator_cut(ctx: Context<SetCreatorCut>, bps: u16) -> Result<()> {
        instructions::creator_cut::set_creator_cut(ctx, bps)
    }

    /// Asks for a higher cut on a basket with shares out, applying a day from now. Creator only.
    pub fn propose_cut_rise(ctx: Context<ProposeCutRise>, bps: u16) -> Result<()> {
        instructions::creator_cut::propose_cut_rise(ctx, bps)
    }

    /// Applies a rise whose day's notice has run out, unless the cut was set or lowered since it
    /// was asked for. Permissionless.
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

    /// Posts an asset's USD price (per whole token, x1e12). Bounded by its guard: within Pyth's
    /// confidence of a fresh Pyth price where the asset has a feed, else within 20% of an hourly
    /// reference, which an asset's first price only seeds.
    pub fn set_price(ctx: Context<SetPrice>, price_e12: u64) -> Result<()> {
        instructions::protocol::set_price(ctx, price_e12)
    }

    /// Names the Pyth feed an asset's prices are checked against (all zeros for none).
    /// Protocol authority only.
    pub fn set_price_feed(ctx: Context<SetPriceFeed>, feed_id: [u8; 32]) -> Result<()> {
        instructions::protocol::set_price_feed(ctx, feed_id)
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

    /// Marks an asset GraviFi Verified, or takes the mark away. A smart basket holds an asset
    /// without it to at most its Unverified cap. Protocol authority only.
    pub fn set_verified(ctx: Context<SetVerified>, verified: bool) -> Result<()> {
        instructions::smart::set_verified(ctx, verified)
    }

    /// Makes a creator-managed basket smart, before its first deposit, with its mandate's hash
    /// and guardrails, burning the smart burn (`set_param` CONFIG_SMART_BURN) from the creator's GRAVI.
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

    // ------------------------------------------------------------------ extension and splits

    /// Hands the basket to another wallet, which then holds every creator power and payout. The
    /// manager now signs (the creator, until a first handover). `creator` stays the PDA's seed.
    pub fn hand_over(ctx: Context<HandOver>, new_manager: Pubkey) -> Result<()> {
        instructions::basket::hand_over(ctx, new_manager)
    }

    /// Creates a basket's extension account (`ext.rs`), the payer paying its rent. Permissionless.
    pub fn ext_init(ctx: Context<ExtInit>) -> Result<()> {
        instructions::basket::ext_init(ctx)
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
