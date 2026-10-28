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
    /// promise of that mode.
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
    /// `max_slippage_bps`, came back as GRAVI. `refund`: as in `rebalance_start`, from the
    /// burn's tokens, for the buyback's network costs.
    pub fn buyback_start(ctx: Context<BuybackStart>, sell_amount: u64, max_slippage_bps: u16, refund: u64) -> Result<()> {
        instructions::buyback::buyback_start(ctx, sell_amount, max_slippage_bps, refund)
    }

    /// Second half of a buyback slice: checks the GRAVI that came back clears the oracle
    /// floor, then burns every GRAVI the burn PDA holds.
    pub fn buyback_finish(ctx: Context<BuybackFinish>) -> Result<()> {
        instructions::buyback::buyback_finish(ctx)
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
}
