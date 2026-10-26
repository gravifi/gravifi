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
/// authority can raise it, never lower it. The one exception is a referred wallet's first
/// month, which pays half the fee and burns none of it (`Invite`).
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
/// The GRAVI a creator burns for each asset in a basket: a singleton at `[CREATION_BURN_SEED]`.
pub const CREATION_BURN_SEED: &[u8] = b"creation_burn";
/// The most GRAVI the protocol authority can ask a creator to burn per asset, in whole tokens.
/// The authority moves the amount within it; only an upgrade moves the ceiling.
pub const MAX_BURN_PER_ASSET_TOKENS: u64 = 1_000_000;

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
    /// oracle. Mint and redeem re-derive these units from the vault's balance and the
    /// share supply before using them (`Basket::live_units`), so tokens that reached the
    /// vault outside the program (an airdrop, a donation, a sleeve a redeemer left
    /// behind) belong to the holders at that moment and can't be bought at the old price.
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

    /// Units one share of a sleeve is worth right now, to store: what its vault holds over the
    /// share supply, rounded down. With no shares out there is nothing to divide, and the
    /// stored units stand (a seed sets them). Past u64 (a dust supply and a vault that isn't)
    /// it holds at u64::MAX rather than fail, so nothing that stores units can be stopped by a
    /// donation; nothing prices from it then (see `slice_of`).
    pub fn live_units(vault_amount: u64, supply: u64, stored: u64) -> Result<u64> {
        if supply == 0 {
            return Ok(stored);
        }
        let units = (vault_amount as u128) * ONE_SHARE / supply as u128;
        Ok(u64::try_from(units).unwrap_or(u64::MAX))
    }

    /// The slice of a vault holding `vault_amount` that `shares` share base units are worth
    /// with `supply` out, rounded up for a deposit and down for a payout. Mint, redeem and the
    /// buyback's redeem price from this, never from stored units that may predate a balance
    /// change (priced from stale units, a surplus could be minted into, booked by `sync_units`
    /// and redeemed in one transaction, taking the holders' share of it).
    ///
    /// It goes through the vault's units per share, as the app quotes them, while those fit in
    /// u64. The units are rounded down first, so a deposit can fall short of vault × shares ÷
    /// supply by up to one base unit per whole share: next to the protocol fee on every mint
    /// and redeem, too little to profit from on any basket holding more than a few hundred base
    /// units of each token per share. Past u64 (a dust supply and a vault that isn't) it's
    /// vault × shares ÷ supply straight, so a redeem can't fail there: a payout is never more
    /// than the vault holds.
    pub fn slice_of(vault_amount: u64, shares: u64, supply: u64, round_up: bool) -> Result<u64> {
        require!(supply > 0, crate::errors::VaultError::ZeroSupply);
        let units = (vault_amount as u128) * ONE_SHARE / supply as u128;
        if let Ok(units) = u64::try_from(units) {
            return Self::amount_for(units, shares, round_up);
        }
        let n = (vault_amount as u128) * (shares as u128);
        let q = if round_up { n.div_ceil(supply as u128) } else { n / supply as u128 };
        u64::try_from(q).map_err(|_| crate::errors::VaultError::MathOverflow.into())
    }

    /// Base units of a constituent required to back `shares` share base units.
    ///
    /// `round_up` is the asymmetry that keeps the vault solvent: minting rounds the
    /// deposit up so the vault never under-collects on these units, redeeming rounds the
    /// payout down so it never over-pays. Each rounds in the vault's favour by at most one
    /// base unit, for the units it's given (see `slice_of` for how those are rounded).
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

/// GRAVI burned for each asset a basket holds, at `[CREATION_BURN_SEED]`: `initialize_basket`
/// burns it for every asset the basket starts with, `add_constituent` for each one added later.
/// The protocol authority sets it (`set_creation_burn`), up to `MAX_BURN_PER_ASSET_TOKENS`; 0
/// turns it off. Paid on creating, so a basket that's never seeded has still paid it. Both
/// instructions name it, so a cluster needs it set once before baskets can be created.
#[account]
#[derive(InitSpace)]
pub struct CreationBurn {
    pub bump: u8,
    /// GRAVI base units burned from the creator's account per asset.
    pub per_asset: u64,
    /// GRAVI burned this way, all time, in base units.
    pub total_burned: u64,
    /// Baskets that paid it.
    pub baskets: u32,
}

/// The protocol account may carry, after its fields, one wallet whose baskets burn no GRAVI (the
/// team's, for its own baskets: `set_burn_exempt`). Kept outside the struct so the account, and
/// every instruction naming it, reads as before: Anchor writes back only the struct's bytes, and
/// the account is only ever created once (`init_protocol`), so nothing checks its size.
pub const BURN_EXEMPT_OFFSET: usize = 8 + Protocol::INIT_SPACE;

pub fn burn_exempt(protocol: &AccountInfo) -> Option<Pubkey> {
    let data = protocol.try_borrow_data().ok()?;
    let bytes: [u8; 32] = data.get(BURN_EXEMPT_OFFSET..BURN_EXEMPT_OFFSET + 32)?.try_into().ok()?;
    let wallet = Pubkey::new_from_array(bytes);
    (wallet != Pubkey::default()).then_some(wallet)
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
/// Only `register_asset` writes AssetEntry accounts (only this program can write data to an
/// account it owns), always at `[ASSET_SEED, mint]`, and callers check `entry.mint` against
/// the mint they expect. The address is pinned to that PDA as well, with the stored bump,
/// so provenance doesn't rest on that reasoning alone.
pub fn load_asset_entry(ai: &AccountInfo) -> Result<AssetEntry> {
    require_keys_eq!(*ai.owner, crate::ID, VaultError::AssetNotListed);
    let data = ai.try_borrow_data()?;
    let entry = AssetEntry::try_deserialize(&mut &data[..])?;
    let expected = Pubkey::create_program_address(&[ASSET_SEED, entry.mint.as_ref(), &[entry.bump]], &crate::ID)
        .map_err(|_| VaultError::AssetNotListed)?;
    require_keys_eq!(*ai.key, expected, VaultError::AssetNotListed);
    Ok(entry)
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
/// vault can never be filled worse than this, however the swap is routed or attacked. 3%: a
/// memecoin trading only in a Pump.fun pool costs more than 1.5% in pool fees alone (NTRNDM
/// 1.62% from SOL), so at 1.5% the keeper could never buy one into a basket. The keeper asks
/// for 1% on slices of stocks and crypto, and up to this only on slices touching a memecoin.
pub const MAX_REBALANCE_SLIPPAGE_BPS: u16 = 300;
/// Ceiling on a buyback slice's tolerance. Wider than a rebalance's: GRAVI first trades on
/// pump.fun's bonding curve, whose fee alone is about 1%, and a buyback spends protocol fees
/// a small slice at a time, never a holder's deposit.
pub const MAX_BUYBACK_SLIPPAGE_BPS: u16 = 300;
/// One slice may move at most this share of the basket's value, so each trade stays small
/// against pool depth and a sandwich costs more than it earns.
pub const MAX_SLICE_BPS: u128 = 500;
/// The most one rebalance, sweep or buyback slice pays the rebalancer back for what running
/// it cost (network fees, and the accounts it opened for the basket), in USD x1e12: $0.05. A
/// slice costs it about 0.00005 SOL; the rest of the room repays an opened account over a few
/// slices. Paid in the sold token on top of the slice, so the basket (or the burn's budget)
/// bears it; a leaked keeper key can take at most this per slice.
pub const MAX_KEEPER_REFUND_E12: u128 = 50_000_000_000;
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

/// Who may post prices and run rebalances: the app's keeper today, on every cluster. What a
/// posted price may be is bounded by its `PriceGuard`.
#[account]
#[derive(InitSpace)]
pub struct Keepers {
    pub bump: u8,
    pub price_authority: Pubkey,
    pub rebalancer: Pubkey,
}

/// A referred wallet's invite: `[INVITE_SEED, wallet]`, written with the rebalancer's signature
/// and paid for by the wallet (`set_invite`), whose rent goes back to it at `close_invite`.
pub const INVITE_SEED: &[u8] = b"invite";
/// The longest an invite can run from when it's written: a month, and a day of room.
pub const MAX_INVITE_SECS: i64 = 31 * 86_400;

/// Until `until`, `wallet` pays half the protocol fee on its mints and redeems (rounded up), all
/// of it to the treasury and none to the burn. Passed after the constituent quads.
#[account]
#[derive(InitSpace)]
pub struct Invite {
    pub bump: u8,
    pub wallet: Pubkey,
    pub until: i64,
}

pub const GUARD_SEED: &[u8] = b"guard";
/// An asset without a Pyth feed: a posted price may differ from its guard's reference by at most
/// this. With the reference rolling forward once an hour, a price can move about this much an
/// hour and no faster, so a leaked price key can't jump a price at once: it takes hours, time to
/// replace the key.
pub const MAX_PRICE_MOVE_BPS: u128 = 2_000;
/// How often the reference rolls forward to the newest accepted price.
pub const PRICE_REFERENCE_SECS: i64 = 3_600;
/// An asset with a Pyth feed: its price may be at most this many seconds old when a price is
/// posted against it (the sponsored feeds update at least every few minutes).
pub const MAX_PYTH_AGE_SECS: i64 = 300;
/// ...and the posted price must be within three times Pyth's confidence of it, but at least
/// 3% and at most 5%: room for the difference between Pyth and the pools the keeper trades.
pub const PYTH_TOLERANCE_MIN_BPS: u128 = 300;
pub const PYTH_TOLERANCE_MAX_BPS: u128 = 500;
pub const PYTH_CONF_MULTIPLE: u128 = 3;

/// What `set_price` checks a posted price against, at `[GUARD_SEED, mint]`: a reference that
/// rolls forward hourly, and the asset's Pyth feed if the protocol authority named one.
#[account]
#[derive(InitSpace)]
pub struct PriceGuard {
    pub bump: u8,
    pub mint: Pubkey,
    /// The price a new one is held to, and when it was accepted; 0 until the first price.
    pub reference_e12: u64,
    pub reference_at: i64,
    /// Pyth feed id; all zeros for none.
    pub pyth_feed: [u8; 32],
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
