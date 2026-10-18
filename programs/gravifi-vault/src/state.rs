use anchor_lang::prelude::*;

use crate::errors::VaultError;

/// Fixed-size constituent array keeps the Basket account a constant size, which
/// avoids realloc paths and makes rent predictable for basket creators.
pub const MAX_CONSTITUENTS: usize = 16;

/// How many constituents a basket may have today.
///
/// Each mint, seed and redeem names every constituent in one transaction; through the
/// basket's lookup table 14 fit with room to spare (tests/_capacity.ts measures them), 16 do
/// not without trimming. Storage keeps room for 16, so raising this later needs no migration.
pub const MAX_LAUNCH_CONSTITUENTS: usize = 14;

/// Hard ceilings on the two fees: the creator's, in bps per year, streamed while shares are
/// held; and the protocol's, in bps of the shares on every mint and every redeem. Holding
/// pays the protocol nothing.
///
/// Governance moves fees *within* these bounds; only a program upgrade can move the
/// bounds themselves. That makes the price a property of the code rather than a promise a
/// future vote could quietly break.
pub const MAX_CREATOR_FEE_BPS: u16 = 20;
pub const MAX_PROTOCOL_FEE_BPS: u16 = 50;
/// The least of every protocol fee that goes to buying GRAVI and burning it: half. The
/// authority can raise it, never lower it.
pub const MIN_BURN_SHARE_BPS: u16 = 5_000;

pub const SHARE_DECIMALS: u8 = 9;
/// Base units in one whole share.
pub const ONE_SHARE: u128 = 1_000_000_000;
pub const BPS_DENOM: u128 = 10_000;
/// Julian year, matching the convention used for annualised fee quotes.
pub const SECONDS_PER_YEAR: i128 = 31_557_600;

pub const PROTOCOL_SEED: &[u8] = b"protocol";
pub const ASSET_SEED: &[u8] = b"asset";
/// Owner of every burn-earmarked account. It signs only inside the buyback instructions,
/// so what it holds can only be redeemed into its own accounts, sold for GRAVI and burned.
pub const BURN_SEED: &[u8] = b"burn";

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum AccessMode {
    /// Anyone may mint.
    Public,
    /// Only wallets on the basket's allowlist may mint.
    Allowlist,
    /// Only the creator may mint. Redemption always stays open to whoever holds shares.
    Private,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum ControlMode {
    /// The creator changes the mandate directly.
    Creator,
    /// Legacy: holder governance, since removed. Kept so baskets created with it still
    /// deserialise; no basket can be created this way any more, and the protocol authority
    /// can return an existing one to its creator (`restore_creator_control`).
    Dao,
    /// Nothing may change the mandate. Enforced here, not merely in the UI.
    Immutable,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum RebalanceMode {
    Periodic,
    Drift,
    Hybrid,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, Debug, InitSpace)]
pub struct Constituent {
    pub mint: Pubkey,
    /// Where this sleeve is supposed to sit, in basis points of the basket.
    pub target_bps: u16,
    /// Base units of `mint` backing one whole share.
    ///
    /// This is the entire mint/redeem contract: deposit `units_per_share * n / ONE_SHARE`
    /// of every constituent and receive `n` share base units, and vice versa. No price
    /// feed is consulted, so minting and redeeming cannot be manipulated by moving an
    /// oracle. Rebalancing changes these units while supply stays constant.
    pub units_per_share: u64,
}

#[account]
#[derive(InitSpace)]
pub struct Basket {
    pub bump: u8,
    pub creator: Pubkey,
    pub share_mint: Pubkey,
    /// Share supply when the current rebalance slice started; the finish requires it
    /// unchanged. (These 32 bytes held the governor's address before holder governance was
    /// removed, so the account layout is unchanged.)
    pub rebalance_supply: u64,
    pub reserved: [u8; 24],
    /// Namespace seed so one creator can run many baskets.
    pub basket_id: [u8; 16],

    pub access: AccessMode,
    pub control: ControlMode,

    pub rebalance_mode: RebalanceMode,
    pub interval_secs: i64,
    pub drift_bps: u16,
    pub last_rebalance: i64,

    pub mgmt_fee_bps: u16,
    pub last_fee_accrual: i64,

    /// Set once by `seed_basket`; mint/redeem are inert until then.
    pub seeded: bool,
    /// Set by `sync_listing_status` when any constituent has been delisted. Stops new
    /// money entering a basket that holds an asset the protocol no longer vouches for;
    /// redemption is unaffected.
    pub mint_paused: bool,
    pub constituent_count: u8,
    #[max_len(MAX_CONSTITUENTS)]
    pub constituents: Vec<Constituent>,
}

impl Basket {
    pub fn active(&self) -> &[Constituent] {
        &self.constituents[..self.constituent_count as usize]
    }

    pub fn signer_seeds<'a>(&'a self, creator: &'a Pubkey, bump: &'a [u8; 1]) -> [&'a [u8]; 4] {
        [b"basket", creator.as_ref(), self.basket_id.as_ref(), bump]
    }

    /// Base units of a constituent required to back `shares` share base units.
    ///
    /// `round_up` is the asymmetry that keeps the vault solvent: minting rounds the
    /// deposit up so the vault never under-collects, redeeming rounds the payout down
    /// so it never over-pays. Both round in the vault's favour by at most one base unit.
    pub fn amount_for(units_per_share: u64, shares: u64, round_up: bool) -> Result<u64> {
        let n = (units_per_share as u128)
            .checked_mul(shares as u128)
            .ok_or(crate::errors::VaultError::MathOverflow)?;
        let q = n / ONE_SHARE;
        let q = if round_up && n % ONE_SHARE != 0 { q + 1 } else { q };
        u64::try_from(q).map_err(|_| crate::errors::VaultError::MathOverflow.into())
    }
}

/// One per (basket, wallet) when access == Allowlist. Existence is the permission.
#[account]
#[derive(InitSpace)]
pub struct AllowEntry {
    pub bump: u8,
    pub basket: Pubkey,
    pub wallet: Pubkey,
}

/// Protocol-wide configuration. Singleton at `[PROTOCOL_SEED]`.
#[account]
#[derive(InitSpace)]
pub struct Protocol {
    pub bump: u8,
    /// Lists assets and sets protocol fees: the team's multisig on mainnet.
    pub authority: Pubkey,
    /// Emergency delisting only; can never list. `Pubkey::default()` means none.
    pub guardian: Pubkey,
    pub gravi_mint: Pubkey,
    /// Protocol fee on every mint and every redeem, in bps of the shares, kept in shares.
    /// Holding pays the protocol nothing.
    pub protocol_fee_bps: u16,
    /// Portion of the protocol fee earmarked for buying back and burning GRAVI; the rest
    /// goes to the treasury.
    pub burn_share_bps: u16,
    pub listed_count: u32,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum AssetStatus {
    Listed,
    Delisted,
}

/// One per mint at `[ASSET_SEED, mint]`. Baskets may only be built from Listed assets.
#[account]
#[derive(InitSpace)]
pub struct AssetEntry {
    pub bump: u8,
    pub mint: Pubkey,
    pub status: AssetStatus,
    pub listed_at: i64,
    pub delisted_at: i64,
}

/// Reads an AssetEntry from an arbitrary account.
///
/// Owner plus discriminator is sufficient provenance: only `register_asset` creates
/// AssetEntry accounts, always at `[ASSET_SEED, mint]`, so there is exactly one per mint
/// and callers additionally check `entry.mint` against the mint they expect.
pub fn load_asset_entry(ai: &AccountInfo) -> Result<AssetEntry> {
    require_keys_eq!(*ai.owner, crate::ID, VaultError::AssetNotListed);
    let data = ai.try_borrow_data()?;
    AssetEntry::try_deserialize(&mut &data[..])
}

// ---------------------------------------------------------------------------- rebalancing

pub const KEEPERS_SEED: &[u8] = b"keepers";
pub const PRICE_SEED: &[u8] = b"price";
pub const REBALANCE_SEED: &[u8] = b"rebalance";
pub const BUYBACK_SEED: &[u8] = b"buyback";

/// Prices are USD per whole token, scaled by 1e12 (so $0.000001 keeps six digits).
pub const PRICE_SCALE: u128 = 1_000_000_000_000;
/// A rebalance may only use prices this fresh.
pub const MAX_PRICE_AGE_SECS: i64 = 120;
/// Ceiling on the tolerance a rebalance slice may accept against the oracle price. The
/// vault can never be filled worse than this, however the swap is routed or attacked.
pub const MAX_REBALANCE_SLIPPAGE_BPS: u16 = 150;
/// Ceiling on a buyback slice's tolerance. Wider than a rebalance's: GRAVI first trades on
/// pump.fun's bonding curve, whose fee alone is about 1%, and a buyback spends protocol fees
/// a small slice at a time, never a holder's deposit.
pub const MAX_BUYBACK_SLIPPAGE_BPS: u16 = 300;
/// One slice may move at most this share of the basket's value, so each trade stays small
/// against pool depth and a sandwich costs more than it earns.
pub const MAX_SLICE_BPS: u128 = 500;
/// Sleeves closer than this to target aren't worth trading.
pub const MIN_TRADE_DRIFT_BPS: i64 = 10;
/// The least a basket's policy may ask for: a drift tolerance under 0.5%, or a schedule more
/// often than daily, would have the keeper trade it (and its holders pay slippage) for noise.
pub const MIN_POLICY_DRIFT_BPS: u16 = 50;
pub const MIN_POLICY_INTERVAL_SECS: i64 = 86_400;
/// A sleeve set to 0% sells its last this-many bps of the basket in one slice, into any
/// sleeve under target, so it empties completely and can be removed.
pub const EMPTYING_BPS: i64 = 50;
/// The highest Token-2022 transfer fee an asset may charge to be listed, added to a basket or
/// bought into one (stonkfun tokens charge 1-3%, PreStocks 1%). A fee raised above it after
/// listing pauses new money into that asset; redemptions and selling it out still work.
pub const MAX_TRANSFER_FEE_BPS: u16 = 300;
/// A rebalance session ends once every sleeve is this close to target.
pub const SESSION_END_DRIFT_BPS: i64 = 25;
/// A session that hasn't traded for about a day (at ~400 ms a slot) lapses: the basket goes
/// back to trading only when its policy says it's due.
pub const SESSION_IDLE_SLOTS: u64 = 216_000;
/// At most one split (direct or reverse) per basket in any 30 days.
pub const SPLIT_COOLDOWN_SECS: i64 = 30 * 86_400;

/// Who may post prices and run rebalances. On the testnet both are the app's keeper; on
/// mainnet prices come from Pyth and the rebalancer role can be opened up.
#[account]
#[derive(InitSpace)]
pub struct Keepers {
    pub bump: u8,
    pub price_authority: Pubkey,
    pub rebalancer: Pubkey,
}

/// Oracle price of one listed asset, at `[PRICE_SEED, mint]`.
#[account]
#[derive(InitSpace)]
pub struct AssetPrice {
    pub bump: u8,
    pub mint: Pubkey,
    /// Copied from the mint, so baskets can be valued without passing every mint in.
    pub decimals: u8,
    pub price_e12: u64,
    pub updated_at: i64,
}

/// Per-basket rebalancing state, at `[REBALANCE_SEED, basket]`.
#[account]
#[derive(InitSpace)]
pub struct RebalanceState {
    pub bump: u8,
    pub basket: Pubkey,
    /// A rebalance was due and hasn't brought every sleeve within SESSION_END_DRIFT_BPS yet.
    pub session_active: bool,
    /// A slice is between `rebalance_start` and `rebalance_finish` (same transaction).
    pub pending: bool,
    pub pending_slot: u64,
    pub sell_index: u8,
    pub buy_index: u8,
    pub sell_amount: u64,
    pub min_buy: u64,
    pub buy_balance_before: u64,
    pub slices: u32,
}

/// The GRAVI buyback-and-burn, a singleton at `[BUYBACK_SEED]`. Works like a rebalance
/// slice: `buyback_start` lends tokens the burn PDA holds to the rebalancer, who swaps them
/// for GRAVI in the same transaction; `buyback_finish` checks at least the oracle value came
/// back and burns every GRAVI the burn PDA holds.
#[account]
#[derive(InitSpace)]
pub struct Buyback {
    pub bump: u8,
    pub pending: bool,
    pub pending_slot: u64,
    pub min_gravi: u64,
    pub gravi_before: u64,
    /// GRAVI burned by buybacks, all time, in base units.
    pub total_burned: u64,
    pub slices: u32,
}
