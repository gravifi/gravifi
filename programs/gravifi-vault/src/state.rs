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
/// The ceiling on a creator's cut of a mint (`creator_cut.rs`), on top of the protocol's fee and
/// outside it, so the burn's share of that fee is untouched. Only an upgrade moves it.
pub const MAX_CREATOR_CUT_BPS: u16 = 60;

pub const SHARE_DECIMALS: u8 = 9;
/// Base units in one whole share.
pub const ONE_SHARE: u128 = 1_000_000_000;
/// Set in `mint_in_kind`'s `min_shares`, the rest of it is a floor on the shares the buyer
/// receives, after the protocol's fee and the creator's cut; clear, it's a floor on the shares
/// minted before them, as it always was.
pub const MIN_NET_FLAG: u64 = 1 << 63;
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
    /// Legacy: holder governance, since removed. Kept so the variants after it keep their
    /// numbers; no basket can be created this way, and none exists on mainnet.
    Dao,
    /// Nothing may change the mandate. Enforced here, not merely in the UI.
    Immutable,
    /// A smart basket: the strategist (the app's key, `Strategist`) sets its targets and adds and
    /// removes assets, only within the guardrails on its `Smart` account; the creator changes
    /// those with a day's notice, and the creator or the guardian can pause it. Mints and redeems
    /// pay the Smart-mode protocol fee (`smart_fee_bps`). Entered with `enable_smart`.
    Smart,
    /// Holders who stake its shares vote on what it holds (`community_propose`). The creator keeps
    /// the fee, which a vote can only lower, and nothing else. Entered with `enter_community`.
    Community,
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
    /// Treasury shares owed to GRAVI stakers (`stake_sweep` moves them).
    pub stake_owed: u64,
    /// 1 once holders run it by vote (`enter_community`). It stays 1 while a vote has handed it
    /// to the strategist, when `control` reads Smart. Read through `by_vote`.
    pub community: u8,
    /// Bytes 0..4: when a vote last added an asset (`community_execute_add`), unix seconds as a
    /// u32, read through `vote_added_at`; 0 for never. Byte 4: 1 once the cut was set or lowered
    /// after the pending rise was asked for, which calls that rise off (`cut_rise_void`).
    pub reserved: [u8; 5],
    /// The creator's cut of every mint by someone else, in bps of the shares (`creator_cut.rs`).
    /// (No basket handed to holder governance (`ControlMode::Dao`) exists on mainnet, whose spare
    /// bytes would hold its governor's address here; none can be created.)
    pub creator_cut_bps: u16,
    /// Treasury shares owed to the creator (`claim_creator_cut` moves them).
    pub creator_unclaimed: u64,
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
    /// The creator's cut of a mint.
    pub fn cut_bps(&self) -> u16 {
        self.creator_cut_bps
    }

    /// Treasury shares owed to the creator.
    pub fn owed_to_creator(&self) -> u64 {
        self.creator_unclaimed
    }

    /// Treasury shares owed to GRAVI stakers.
    pub fn owed_to_stakers(&self) -> u64 {
        self.stake_owed
    }

    /// Holders run this basket by vote: `Community`, or `Smart` after a vote handed it over.
    pub fn by_vote(&self) -> bool {
        self.community == 1 && matches!(self.control, ControlMode::Community | ControlMode::Smart)
    }

    /// When a vote last added an asset to this basket (0: never). Only a Community basket writes it.
    pub fn vote_added_at(&self) -> i64 {
        u32::from_le_bytes([self.reserved[0], self.reserved[1], self.reserved[2], self.reserved[3]]) as i64
    }

    /// Whether the creator's pending cut rise, if any, was called off: the cut was set or lowered
    /// since it was asked for (`propose_cut_rise` clears it).
    pub fn cut_rise_void(&self) -> bool {
        self.reserved[4] != 0
    }

    pub fn set_cut_rise_void(&mut self, void: bool) {
        self.reserved[4] = void as u8;
    }

    pub fn set_vote_added_at(&mut self, now: i64) {
        let at = u32::try_from(now).unwrap_or(u32::MAX).to_le_bytes();
        self.reserved[..4].copy_from_slice(&at);
    }

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
/// The protocol authority sets it (`set_param` CONFIG_CREATION_BURN), up to `MAX_BURN_PER_ASSET_TOKENS`; 0
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
/// team's, for its own baskets: `set_role` ROLE_BURN_EXEMPT). Kept outside the struct so the account, and
/// every instruction naming it, reads as before: Anchor writes back only the struct's bytes, and
/// the account is only ever created once (`init_protocol`), so nothing checks its size.
pub const BURN_EXEMPT_OFFSET: usize = 8 + Protocol::INIT_SPACE;

/// The burn-exempt wallet in force, if any (a wallet named waits `ROLE_NOTICE_SECS`).
pub fn burn_exempt(protocol: &AccountInfo) -> Option<Pubkey> {
    let wallet = key_in_force(protocol, BURN_EXEMPT_OFFSET, PENDING_ROLES_OFFSET + 2 * PENDING_KEY_LEN);
    (wallet != Pubkey::default()).then_some(wallet)
}

// ------------------------------------------------------------- changes waiting their notice

/// How long a key named for a role (`set_role`) waits before it acts: a day, so a key swapped in
/// is public before it can do anything. Clearing a role applies at once (an emergency stop).
pub const ROLE_NOTICE_SECS: i64 = 86_400;
/// How long a fee or burn raise (`set_param` CONFIG_*) waits: the week user-facing params wait
/// (`params::PARAM_NOTICE_SECS`), so anyone who'd pay more can leave first. Lowering is at once.
pub const FEE_NOTICE_SECS: i64 = 7 * 86_400;
/// A key waiting to replace a role's: [key 32][applies_at i64], applies_at 0 for none.
pub const PENDING_KEY_LEN: usize = 40;
/// A fee waiting to replace one in force: [value u64][applies_at i64], applies_at 0 for none.
pub const PENDING_FEE_LEN: usize = 16;
/// In the protocol account's tail after the flags: the fee and burn raises waiting, one slot each
/// (`FEE_*`), then the keys waiting for the authority, the guardian and the burn-exempt wallet.
pub const PENDING_FEES_OFFSET: usize = PROTOCOL_FLAGS_OFFSET + 1;
/// The protocol fee and burn share, as `set_param` packs them: `fee | burn_share << 16`.
pub const FEE_PROTOCOL: usize = 0;
pub const FEE_CREATION_BURN: usize = 1;
pub const FEE_SMART: usize = 2;
pub const FEE_SMART_BURN: usize = 3;
pub const PENDING_ROLES_OFFSET: usize = PENDING_FEES_OFFSET + 4 * PENDING_FEE_LEN;
/// The protocol account's full length with everything kept after its fields.
pub const PROTOCOL_TAIL_END: usize = PENDING_ROLES_OFFSET + 3 * PENDING_KEY_LEN;
/// Where the protocol account keeps the authority and the guardian: after its discriminator and bump.
pub const AUTHORITY_AT: usize = 9;
pub const GUARDIAN_AT: usize = 41;
/// The lister's and strategist's accounts: [discriminator][bump][key][pending key], `ROLE_ACCOUNT_LEN`.
pub const ROLE_KEY_AT: usize = 9;
pub const ROLE_PENDING_AT: usize = 41;
pub const ROLE_ACCOUNT_LEN: usize = ROLE_PENDING_AT + PENDING_KEY_LEN;
/// The keepers' account: [discriminator][bump][price authority][rebalancer], then each one's
/// pending key, `KEEPERS_LEN`.
pub const PRICE_AUTHORITY_AT: usize = 9;
pub const REBALANCER_AT: usize = 41;
pub const PRICE_AUTHORITY_PENDING_AT: usize = 73;
pub const REBALANCER_PENDING_AT: usize = PRICE_AUTHORITY_PENDING_AT + PENDING_KEY_LEN;
pub const KEEPERS_LEN: usize = REBALANCER_PENDING_AT + PENDING_KEY_LEN;

fn clock_now() -> i64 {
    // 0 if the clock can't be read: nothing waiting has come due then.
    Clock::get().map(|c| c.unix_timestamp).unwrap_or(0)
}

fn word(d: &[u8], o: usize) -> i64 {
    i64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

/// The key a role acts with: the one at `at`, or the pending one at `pending_at` once its day has
/// passed. An account without the pending slot (from before it existed) has nothing waiting, and
/// one too short for the key has none (`Pubkey::default()`, which nobody can sign as).
#[inline(never)]
pub fn key_in_force(ai: &AccountInfo, at: usize, pending_at: usize) -> Pubkey {
    let Ok(d) = ai.try_borrow_data() else { return Pubkey::default() };
    if let Some(s) = d.get(pending_at..pending_at + PENDING_KEY_LEN) {
        let due = word(s, 32);
        if due != 0 && clock_now() >= due {
            return Pubkey::new_from_array(s[..32].try_into().unwrap());
        }
    }
    d.get(at..at + 32).map(|k| Pubkey::new_from_array(k.try_into().unwrap())).unwrap_or_default()
}

/// The protocol authority in force.
pub fn authority_of(protocol: &AccountInfo) -> Pubkey {
    key_in_force(protocol, AUTHORITY_AT, PENDING_ROLES_OFFSET)
}

/// The guardian in force (`Pubkey::default()`: none).
pub fn guardian_of(protocol: &AccountInfo) -> Pubkey {
    key_in_force(protocol, GUARDIAN_AT, PENDING_ROLES_OFFSET + PENDING_KEY_LEN)
}

/// The lister's or the strategist's key in force, from its account.
pub fn role_key(ai: &AccountInfo) -> Pubkey {
    key_in_force(ai, ROLE_KEY_AT, ROLE_PENDING_AT)
}

/// The price authority in force, from the keepers' account.
pub fn price_authority_of(keepers: &AccountInfo) -> Pubkey {
    key_in_force(keepers, PRICE_AUTHORITY_AT, PRICE_AUTHORITY_PENDING_AT)
}

/// The rebalancer in force, from the keepers' account.
pub fn rebalancer_of(keepers: &AccountInfo) -> Pubkey {
    key_in_force(keepers, REBALANCER_AT, REBALANCER_PENDING_AT)
}

/// Names `key` for a role whose key sits at `at` in `data` and its pending one at `pending_at`.
/// A pending key whose day has passed becomes the key first. Clearing the role
/// (`Pubkey::default()`) or naming the key in force applies at once and withdraws any key waiting;
/// any other key waits `ROLE_NOTICE_SECS`. `data` must hold both slots.
pub fn name_key(data: &mut [u8], at: usize, pending_at: usize, key: Pubkey, now: i64) {
    let due = word(data, pending_at + 32);
    if due != 0 && now >= due {
        data.copy_within(pending_at..pending_at + 32, at);
    }
    let current = Pubkey::new_from_array(data[at..at + 32].try_into().unwrap());
    if key == Pubkey::default() || key == current {
        data[at..at + 32].copy_from_slice(key.as_ref());
        data[pending_at..pending_at + PENDING_KEY_LEN].fill(0);
    } else {
        data[pending_at..pending_at + 32].copy_from_slice(key.as_ref());
        data[pending_at + 32..pending_at + PENDING_KEY_LEN].copy_from_slice(&(now + ROLE_NOTICE_SECS).to_le_bytes());
    }
}

/// A fee raise in the protocol account's tail whose week has passed, if any.
pub fn fee_due(protocol: &AccountInfo, slot: usize) -> Option<u64> {
    let d = protocol.try_borrow_data().ok()?;
    let o = PENDING_FEES_OFFSET + slot * PENDING_FEE_LEN;
    let s = d.get(o..o + PENDING_FEE_LEN)?;
    let due = word(s, 8);
    (due != 0 && clock_now() >= due).then(|| word(s, 0) as u64)
}

/// The protocol fee and the burn's share of it in force: `(fee_bps, burn_share_bps)`.
pub fn protocol_fees(protocol: &Account<Protocol>) -> (u16, u16) {
    match fee_due(&protocol.to_account_info(), FEE_PROTOCOL) {
        Some(v) => (v as u16, (v >> 16) as u16),
        None => (protocol.protocol_fee_bps, protocol.burn_share_bps),
    }
}

/// The GRAVI burned per asset on creating a basket, in force.
pub fn creation_burn_per_asset(protocol: &AccountInfo, c: &CreationBurn) -> u64 {
    fee_due(protocol, FEE_CREATION_BURN).unwrap_or(c.per_asset)
}

/// The one key, besides the protocol authority, that may list assets (`register_asset`), at
/// `[LISTER_SEED]`: the app's listing key, which co-signs a listing inside the creator's own
/// transaction once the token passed the app's checks (open listing). It can do nothing else: not
/// delist, not change a setting, not move a token. `set_role` (ROLE_LISTER) names it (`Pubkey::default()`
/// removes it), protocol authority only.
pub const LISTER_SEED: &[u8] = b"lister";

#[account]
#[derive(InitSpace)]
pub struct Lister {
    pub bump: u8,
    pub lister: Pubkey,
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

// --------------------------------------------------------------------------- smart baskets

/// The protocol fee on a smart basket's mints and redeems, in bps of the shares: double the
/// normal one, for the daily reviews. It sits after the burn-exempt wallet, in the protocol
/// account's tail, written by `set_param` (CONFIG_SMART_FEE); until then it's `SMART_FEE_DEFAULT_BPS`.
pub const SMART_FEE_OFFSET: usize = BURN_EXEMPT_OFFSET + 32;
/// [fee u16 little-endian][1 once set].
pub const SMART_FEE_LEN: usize = 3;
pub const SMART_FEE_DEFAULT_BPS: u16 = 80;
/// The ceiling on the Smart-mode fee, as `MAX_PROTOCOL_FEE_BPS` is on the normal one.
pub const MAX_SMART_FEE_BPS: u16 = 100;

/// The GRAVI, in base units, a creator burns once when a basket is made smart (`enable_smart`), on
/// top of the per-asset creation burn. It sits after the Smart-mode fee in the protocol account's
/// tail, written by `set_param` (CONFIG_SMART_BURN); 0, or no tail yet, means none.
pub const SMART_BURN_OFFSET: usize = SMART_FEE_OFFSET + SMART_FEE_LEN;
pub const SMART_BURN_LEN: usize = 8;
/// Its ceiling, in whole GRAVI.
pub const MAX_SMART_BURN_TOKENS: u64 = 1_000_000;

/// The smart burn in force (a raise waits `FEE_NOTICE_SECS`).
pub fn smart_burn(protocol: &AccountInfo) -> u64 {
    if let Some(v) = fee_due(protocol, FEE_SMART_BURN) {
        return v;
    }
    let Ok(data) = protocol.try_borrow_data() else { return 0 };
    data.get(SMART_BURN_OFFSET..SMART_BURN_OFFSET + SMART_BURN_LEN)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

/// The Smart-mode fee in force (a raise waits `FEE_NOTICE_SECS`).
pub fn smart_fee_bps(protocol: &AccountInfo) -> u16 {
    if let Some(v) = fee_due(protocol, FEE_SMART) {
        return v as u16;
    }
    let Ok(data) = protocol.try_borrow_data() else { return SMART_FEE_DEFAULT_BPS };
    match data.get(SMART_FEE_OFFSET..SMART_FEE_OFFSET + SMART_FEE_LEN) {
        Some([lo, hi, 1]) => u16::from_le_bytes([*lo, *hi]),
        _ => SMART_FEE_DEFAULT_BPS,
    }
}

/// The strategist, at `[STRATEGIST_SEED]`: the app's key that runs smart baskets. It may set a
/// smart basket's targets, add and remove its assets and put the mandate's stop on them, only
/// within the basket's guardrails. It can't move a token, change a fee or a guardrail, or touch
/// any other basket. `set_role` (ROLE_STRATEGIST) names it (`Pubkey::default()` removes it), protocol
/// authority only.
pub const STRATEGIST_SEED: &[u8] = b"strategist";

#[account]
#[derive(InitSpace)]
pub struct Strategist {
    pub bump: u8,
    pub strategist: Pubkey,
}

/// `[VERIFIED_SEED, mint]` exists for each asset the protocol authority vouches for (GraviFi
/// Verified, `set_verified`). A smart basket holds an asset without one to at most its guard's
/// `max_unverified_bps`.
pub const VERIFIED_SEED: &[u8] = b"verified";

#[account]
#[derive(InitSpace)]
pub struct VerifiedAsset {
    pub bump: u8,
    pub mint: Pubkey,
}

pub const SMART_SEED: &[u8] = b"smart";
/// Ceilings on a smart basket's guardrails: a creator may set tighter ones, never looser. Target changes (the sum of the increases) per rolling 24 hours and 7 days, and the
/// weight of one asset, Verified and not.
/// (The default of the `SMART_MAX_*` params: `params.rs`.)
pub const SMART_MAX_DAY_BPS: u16 = 4_000;
pub const SMART_MAX_WEEK_BPS: u16 = 10_000;
pub const SMART_MAX_WEIGHT_BPS: u16 = 3_500;
pub const SMART_MAX_UNVERIFIED_BPS: u16 = 3_000;
/// How long a new mandate waits before it applies, so holders who disagree can redeem first.
/// (The default of the `MANDATE_NOTICE_SECS` param: `params.rs`.)
pub const MANDATE_NOTICE_SECS: i64 = 86_400;
/// The turnover window: one bucket per hour, 7 days.
pub const TURNOVER_HOURS: usize = 168;

/// What the strategist must stay within, set by the creator.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, Debug, PartialEq, Eq, InitSpace)]
pub struct Guardrails {
    /// Most the targets may change (sum of the increases) in any 24 hours and any 7 days.
    pub max_day_bps: u16,
    pub max_week_bps: u16,
    /// Most one asset may weigh, and one without a Verified record.
    pub max_weight_bps: u16,
    pub max_unverified_bps: u16,
    /// How many assets may have a target above 0%: 2 to MAX_LAUNCH_CONSTITUENTS (14).
    pub min_assets: u8,
    pub max_assets: u8,
    /// The stop the strategist puts on an asset (`smart_set_stop`): this far below the price
    /// when set, trailing or not. 0: none.
    pub stop_bps: u16,
    pub stop_trailing: bool,
}

/// Who paused a smart basket.
pub const PAUSED_BY_CREATOR: u8 = 1;
pub const PAUSED_BY_GUARDIAN: u8 = 2;

/// A smart basket's rules, at `[SMART_SEED, basket]`.
#[account]
#[derive(InitSpace)]
pub struct Smart {
    pub bump: u8,
    pub basket: Pubkey,
    /// sha256 of the mandate the app reviews the basket against (its JSON), so holders can
    /// check which rules it follows.
    pub mandate_hash: [u8; 32],
    pub guard: Guardrails,
    /// 0 while running; else who paused it (PAUSED_BY_*). Paused, the strategist can't act and
    /// the basket stays on its last targets; stops and rebalancing go on.
    pub paused_by: u8,
    /// A mandate the creator proposed, applying from `pending_at` (0: none). `pending_leave`
    /// returns the basket to its creator's hands instead.
    pub pending_at: i64,
    pub pending_hash: [u8; 32],
    pub pending_guard: Guardrails,
    pub pending_leave: bool,
    /// Target changes per hour over the last `TURNOVER_HOURS`, bucket `hour % TURNOVER_HOURS`;
    /// `hour` is the newest hour (unix time / 3600) the buckets reach.
    pub hour: i64,
    pub turnover: [u16; TURNOVER_HOURS],
    /// Target changes the strategist made, all time.
    pub changes: u32,
}

/// Moves a turnover window up to `now_hour`, emptying the buckets of hours that left it.
pub fn advance_window(hour: &mut i64, turnover: &mut [u16; TURNOVER_HOURS], now_hour: i64) {
    if now_hour <= *hour {
        return;
    }
    if now_hour - *hour >= TURNOVER_HOURS as i64 {
        *turnover = [0; TURNOVER_HOURS];
    } else {
        for h in (*hour + 1)..=now_hour {
            turnover[h.rem_euclid(TURNOVER_HOURS as i64) as usize] = 0;
        }
    }
    *hour = now_hour;
}

/// Changes in the last 24 hours (this one included) and the last 7 days, once advanced.
pub fn window_used(hour: i64, turnover: &[u16; TURNOVER_HOURS]) -> (u32, u32) {
    let day = (0..24).map(|k| turnover[(hour - k).rem_euclid(TURNOVER_HOURS as i64) as usize] as u32).sum();
    let week = turnover.iter().map(|t| *t as u32).sum();
    (day, week)
}

impl Smart {
    pub fn advance(&mut self, now_hour: i64) {
        advance_window(&mut self.hour, &mut self.turnover, now_hour)
    }

    pub fn used(&self) -> (u32, u32) {
        window_used(self.hour, &self.turnover)
    }
}

// ---------------------------------------------------------------------------- rebalancing

pub const KEEPERS_SEED: &[u8] = b"keepers";
pub const PRICE_SEED: &[u8] = b"price";
pub const REBALANCE_SEED: &[u8] = b"rebalance";
pub const BUYBACK_SEED: &[u8] = b"buyback";
pub const BUYBACK_SOL_SEED: &[u8] = b"buyback_sol";
/// Wrapped SOL: what the burn PDA's tokens are sold for, before the SOL buys GRAVI in one go.
pub const NATIVE_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");

/// Prices are USD per whole token, scaled by 1e12 (so $0.000001 keeps six digits).
pub const PRICE_SCALE: u128 = 1_000_000_000_000;
/// A rebalance may only use prices this fresh.
/// (The default of the `PRICE_AGE_SECS` param: `params.rs`.)
pub const MAX_PRICE_AGE_SECS: i64 = 120;
/// Ceiling on the tolerance a rebalance slice may accept against the oracle price. The
/// vault can never be filled worse than this, however the swap is routed or attacked. 3%: a
/// memecoin trading only in a Pump.fun pool costs more than 1.5% in pool fees alone (NTRNDM
/// 1.62% from SOL), so at 1.5% the keeper could never buy one into a basket. The keeper asks
/// for 1% on slices of stocks and crypto, and up to this only on slices touching a memecoin.
/// (The default of the `REBALANCE_SLIPPAGE_BPS` param: `params.rs`.)
pub const MAX_REBALANCE_SLIPPAGE_BPS: u16 = 300;
/// Ceiling on a buyback slice's tolerance, both its sales to SOL and its GRAVI buy. Far wider
/// than a rebalance's: a buyback spends protocol fees, never a holder's deposit, and its fee
/// tokens are memecoins a few pools from SOL whose fees alone pass 3% (31 Oct, the pump "LTC":
/// 3.7% to GRAVI through three pools): sell it all, slippage aside, and burn.
/// (The default of the `BUYBACK_SLIPPAGE_BPS` param: `params.rs`.)
pub const MAX_BUYBACK_SLIPPAGE_BPS: u16 = 1_000;
/// One slice may move at most this share of the basket's value, so each trade stays small
/// against pool depth and a sandwich costs more than it earns.
/// (The default of the `SLICE_BPS` param: `params.rs`.)
pub const MAX_SLICE_BPS: u128 = 500;
/// A sleeve set to 0% (stopped out, or taken out by its creator) may sell up to this much of the
/// basket in one slice ("clip"), so it leaves in minutes rather than hours. Each clip is still
/// held to the oracle floor and stops at the bought sleeve's target.
/// (The default of the `LEAVING_SLICE_BPS` param: `params.rs`.)
pub const MAX_LEAVING_SLICE_BPS: u128 = 5_000;
/// The most one rebalance, sweep or buyback slice pays the rebalancer back for what running
/// it cost (network fees, and the accounts it opened for the basket), in USD x1e12: $0.05. A
/// slice costs it about 0.00005 SOL; the rest of the room repays an opened account over a few
/// slices. Paid in the sold token on top of the slice, so the basket (or the burn's budget)
/// bears it; a leaked keeper key can take at most this per slice.
/// (The default of the `KEEPER_REFUND_E12` param: `params.rs`.)
pub const MAX_KEEPER_REFUND_E12: u128 = 50_000_000_000;
/// The most one buyback slice pays the rebalancer back, in USD x1e12: $0.50. A buyback run
/// sells every token the burn PDA holds for SOL, and the rebalancer is paid back for all of it
/// at once, in SOL, out of the one GRAVI buy that follows (some 25 transactions of fees). Only
/// ever from the burn's budget.
/// (The default of the `BUYBACK_REFUND_E12` param: `params.rs`.)
pub const MAX_BUYBACK_REFUND_E12: u128 = 500_000_000_000;
/// Sleeves closer than this to target aren't worth trading.
pub const MIN_TRADE_DRIFT_BPS: i64 = 10;
/// The least a basket's policy may ask for: a drift tolerance under 0.5%, or a schedule more
/// often than daily, would have the keeper trade it (and its holders pay slippage) for noise.
/// (The default of the `POLICY_MIN_DRIFT_BPS` param: `params.rs`.)
pub const MIN_POLICY_DRIFT_BPS: u16 = 50;
/// (The default of the `POLICY_MIN_INTERVAL_SECS` param: `params.rs`.)
pub const MIN_POLICY_INTERVAL_SECS: i64 = 86_400;
/// A sleeve set to 0% sells its last this-many bps of the basket in one slice, into any
/// sleeve under target, so it empties completely and can be removed.
pub const EMPTYING_BPS: i64 = 50;
/// The highest Token-2022 transfer fee an asset may charge to be listed, added to a basket or
/// bought into one (stonkfun tokens charge 1-3%, PreStocks 1%; 4% leaves room for tokens taking
/// 3% on trades). A fee raised above it after listing pauses new money into that asset;
/// redemptions and selling it out still work.
/// (The default of the `TRANSFER_FEE_CAP_BPS` param: `params.rs`.)
pub const MAX_TRANSFER_FEE_BPS: u16 = 400;
/// A rebalance session ends once every sleeve is this close to target.
pub const SESSION_END_DRIFT_BPS: i64 = 25;
/// A session that hasn't traded for about a day (at ~400 ms a slot) lapses: the basket goes
/// back to trading only when its policy says it's due.
pub const SESSION_IDLE_SLOTS: u64 = 216_000;
/// At most one split (direct or reverse) per basket in any 30 days.
/// (The default of the `SPLIT_COOLDOWN_SECS` param: `params.rs`.)
pub const SPLIT_COOLDOWN_SECS: i64 = 30 * 86_400;

/// Who may post prices and run rebalances: the app's keeper today, on every cluster. What a
/// posted price may be is bounded by its `PriceGuard`. A key named for either waits a day after the
/// fields, in the account's tail (`KEEPERS_LEN`); every check reads the key in force through
/// `price_authority_of` / `rebalancer_of`, never these fields alone. (The same goes for the lister,
/// the strategist, the authority and the guardian: `role_key`, `authority_of`, `guardian_of`.)
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
/// A pending rise of a basket's creator cut, at `[CUT_NOTICE_SEED, basket]`.
pub const CUT_NOTICE_SEED: &[u8] = b"cut_notice";
/// How long a rise waits once a basket has shares, so no buyer pays more than they were quoted.
/// (The default of the `CUT_NOTICE_SECS` param: `params.rs`.)
pub const CUT_NOTICE_SECS: i64 = 86_400;

#[account]
#[derive(InitSpace)]
pub struct CutNotice {
    pub bump: u8,
    pub basket: Pubkey,
    pub bps: u16,
    /// When it may apply.
    pub at: i64,
}

/// The longest an invite can run from when it's written: a month, and a day of room.
/// (The default of the `INVITE_MAX_SECS` param: `params.rs`.)
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
/// (The default of the `PRICE_MOVE_BPS` param: `params.rs`.)
pub const MAX_PRICE_MOVE_BPS: u128 = 2_000;
/// How often the reference rolls forward to the newest accepted price.
/// (The default of the `PRICE_REFERENCE_SECS` param: `params.rs`.)
pub const PRICE_REFERENCE_SECS: i64 = 3_600;
/// An asset with a Pyth feed: its price may be at most this many seconds old when a price is
/// posted against it (the sponsored feeds update at least every few minutes).
/// (The default of the `PYTH_AGE_SECS` param: `params.rs`.)
pub const MAX_PYTH_AGE_SECS: i64 = 300;
/// ...and the posted price must be within three times Pyth's confidence of it, but at least
/// 3% and at most 5%: room for the difference between Pyth and the pools the keeper trades.
/// (The default of the `PYTH_TOLERANCE_MIN_BPS` param: `params.rs`.)
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

/// A sleeve set to 0% whose price hasn't been posted for this long counts as worth nothing in the
/// books (`value_book`): a token the market stopped pricing would otherwise block every rebalance
/// of the basket. At $0 it can't be sold (its floor is zero), only held and redeemed in kind.
/// (The default of the `DEAD_PRICE_SECS` param: `params.rs`.)
pub const DEAD_PRICE_SECS: i64 = 86_400;

// ---------------------------------------------------------------------------- stop losses

pub const STOP_SEED: &[u8] = b"stop";
/// A stop fires only if the price is still below it this long after it was first seen below.
/// (The default of the `STOP_CONFIRM_SECS` param: `params.rs`.)
pub const STOP_CONFIRM_SECS: i64 = 300;
/// ...and no longer than this after: an arming nobody confirmed in time starts over from the next
/// check below the stop, so "below for five minutes" is two checks five to fifteen minutes apart,
/// not two dips hours apart with nobody looking in between.
/// (The default of the `STOP_WINDOW_SECS` param: `params.rs`.)
pub const STOP_ARM_WINDOW_SECS: i64 = 3 * STOP_CONFIRM_SECS;
/// How far below its reference a stop may sit: 5% to 90%.
/// (The default of the `STOP_MIN_BPS` param: `params.rs`.)
pub const MIN_STOP_BPS: u16 = 500;
pub const MAX_STOP_BPS: u16 = 9_000;

/// One asset's stop: it fires when the asset's posted price is below `reference_e12` less
/// `pct_bps`, twice, STOP_CONFIRM_SECS to STOP_ARM_WINDOW_SECS apart. Its reference is never above
/// the asset's fresh posted price when it's written on a seeded basket (`hold_to_price`), so no
/// stop starts out armed.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, Debug, InitSpace)]
pub struct Stop {
    pub mint: Pubkey,
    pub pct_bps: u16,
    /// The reference rises to a higher price that has held (a trailing stop): the lower of the
    /// posted price and the asset's `PriceGuard` reference, once that reference is at least
    /// STOP_CONFIRM_SECS old (`check_stop`). Otherwise it stays where the creator set it.
    pub trailing: bool,
    /// USD per whole token x1e12, in the same units as `AssetPrice`.
    pub reference_e12: u64,
    /// When the price was first seen below the stop; 0 while it isn't.
    pub armed_at: i64,
}

impl Stop {
    /// The price below which this stop arms, x1e12.
    pub fn level_e12(&self) -> u128 {
        self.reference_e12 as u128 * (BPS_DENOM - self.pct_bps as u128) / BPS_DENOM
    }
}

/// A basket's stop losses, at `[STOP_SEED, basket]`, one per asset at most, keyed by mint (removing
/// an asset shifts the indices of those after it). The creator writes them (`set_stops`); anyone
/// may check one against its posted price (`check_stop`), and a stop that fires sets its asset's
/// target to 0% and scales the others up, after which rebalancing sells the asset out.
#[account]
#[derive(InitSpace)]
pub struct StopLoss {
    pub bump: u8,
    pub basket: Pubkey,
    #[max_len(MAX_CONSTITUENTS)]
    pub stops: Vec<Stop>,
    /// Stops that fired, all time.
    pub fired: u32,
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

/// The burn PDA's sales to SOL, a singleton at `[BUYBACK_SOL_SEED]`: `buyback_sol_start` lends a
/// token the burn PDA holds to the rebalancer, who swaps it for wrapped SOL delivered to the burn
/// PDA's own SOL account in the same transaction; `buyback_sol_finish` checks at least the oracle
/// value came back. Once its tokens are all SOL, the SOL buys GRAVI in one go: `buyback_start`
/// with SOL as the token sold, then `buyback_finish` burns it.
#[account]
#[derive(InitSpace)]
pub struct BuybackSol {
    pub bump: u8,
    pub pending: bool,
    pub pending_slot: u64,
    pub min_sol: u64,
    pub sol_before: u64,
    /// SOL these sales brought the burn PDA, all time, in lamports.
    pub total_sol: u64,
    pub slices: u32,
}

// ------------------------------------------------------------------------------ GRAVI staking

/// Flags in the protocol account's tail, after the smart burn. Staking on means every protocol fee
/// owes stakers their share, and it can't be turned off. Community open lets baskets enter
/// Community mode; closing it stops new ones, and those already in carry on.
pub const PROTOCOL_FLAGS_OFFSET: usize = SMART_BURN_OFFSET + SMART_BURN_LEN;
pub const FLAG_STAKING: u8 = 1;
pub const FLAG_COMMUNITY: u8 = 2;

pub fn protocol_flags(protocol: &AccountInfo) -> u8 {
    let Ok(data) = protocol.try_borrow_data() else { return 0 };
    data.get(PROTOCOL_FLAGS_OFFSET).copied().unwrap_or(0)
}

/// The state of GRAVI staking, a singleton at `[STAKE_POOL_SEED]`, and the owner of the vault of
/// staked GRAVI, which only a staker's own withdrawal moves.
pub const STAKE_POOL_SEED: &[u8] = b"stake_pool";
/// Owner of what stakers are paid from: the shares swept from the treasury, the tokens they redeem
/// into, and the SOL pot. It holds no staked GRAVI, so a keeper selling for it can't reach any.
pub const STAKE_REWARDS_SEED: &[u8] = b"stake_rewards";
pub const STAKE_SEED: &[u8] = b"stake";
/// Stakers' share of every protocol fee, out of the treasury's side, in bps of the fee.
pub const STAKER_SHARE_BPS: u128 = 1_000;
/// Each this much warmed-up GRAVI staked gives one basket a day with no GRAVI burned (the
/// threshold). The authority moves it up to the ceiling.
pub const STAKE_THRESHOLD_TOKENS: u64 = 1_000_000;
pub const MAX_STAKE_THRESHOLD_TOKENS: u64 = 10_000_000;
/// A raised threshold applies after this notice, so stakers can top up first.
pub const STAKE_THRESHOLD_NOTICE_SECS: i64 = 7 * 86_400;
/// How long staked GRAVI waits before it counts, for the pot and for free baskets.
/// (The default of the `STAKE_WARMUP_SECS` param: `params.rs`.)
pub const STAKE_WARMUP_SECS: i64 = 2 * 86_400;
pub const DAY_SECS: i64 = 86_400;
/// How long unstaked GRAVI waits before it can be withdrawn, earning nothing.
/// (The default of the `STAKE_UNSTAKE_SECS` param: `params.rs`.)
pub const STAKE_UNSTAKE_SECS: i64 = 7 * 86_400;
/// How long one month's pot takes to pay out.
/// (The default of the `STAKE_PERIOD_SECS` param: `params.rs`.)
pub const STAKE_PERIOD_SECS: i64 = 30 * 86_400;
/// The reward index is lamports per GRAVI base unit x1e18; the rate lamports per second x1e9.
pub const STAKE_INDEX_SCALE: u128 = 1_000_000_000_000_000_000;
pub const STAKE_RATE_SCALE: u128 = 1_000_000_000;

#[account]
#[derive(InitSpace)]
pub struct StakePool {
    pub bump: u8,
    pub gravi_mint: Pubkey,
    /// Base units in one GRAVI, for the threshold's ceiling.
    pub one_gravi: u64,
    pub threshold: u64,
    /// A raise waiting out its notice, from `next_threshold_at` (0: none).
    pub next_threshold: u64,
    pub next_threshold_at: i64,
    /// GRAVI that counts right now: the sum of every stake's `counted`.
    pub total_counted: u64,
    pub index: u128,
    pub rate: u128,
    pub stream_end: i64,
    pub last_update: i64,
    /// Lamports promised to stakers, all time, less any no stake was there to earn.
    pub funded: u64,
    /// Lamports stakers have claimed, all time. The pot holds at least `funded - claimed`.
    pub claimed: u64,
    pub periods: u32,
    /// A sale to SOL, as `BuybackSol`.
    pub sale_pending: bool,
    pub sale_slot: u64,
    pub sale_min_sol: u64,
    pub sale_sol_before: u64,
    pub total_sol: u64,
}

impl StakePool {
    pub fn threshold_at(&self, now: i64) -> u64 {
        if self.next_threshold_at != 0 && now >= self.next_threshold_at { self.next_threshold } else { self.threshold }
    }

    /// Brings the index up to `now`. While nothing counts, what the stream would have paid goes
    /// back to the pot for the next month.
    pub fn accrue(&mut self, now: i64) -> Result<()> {
        let to = now.min(self.stream_end);
        if to <= self.last_update {
            return Ok(());
        }
        let paid = self.rate.checked_mul((to - self.last_update) as u128).ok_or(VaultError::MathOverflow)?;
        if self.total_counted > 0 {
            let step = paid
                .checked_mul(STAKE_INDEX_SCALE / STAKE_RATE_SCALE)
                .ok_or(VaultError::MathOverflow)?
                / self.total_counted as u128;
            self.index = self.index.checked_add(step).ok_or(VaultError::MathOverflow)?;
        } else {
            self.funded = self.funded.saturating_sub((paid / STAKE_RATE_SCALE) as u64);
        }
        self.last_update = to;
        Ok(())
    }
}

/// One wallet's stake, at `[STAKE_SEED, wallet]`.
#[account]
#[derive(InitSpace)]
pub struct Stake {
    pub bump: u8,
    pub wallet: Pubkey,
    /// All of it that's staked, warmed up or not.
    pub amount: u64,
    /// What was staked in the last warm-up and doesn't count yet, and when the newest of it came in,
    /// written so that `pending_at + STAKE_WARMUP_SECS` is when it counts (with the warm-up param
    /// changed, it's shifted by the difference): a top-up restarts the wait for what's still
    /// warming, never for what counts.
    pub pending: u64,
    pub pending_at: i64,
    /// How much counts toward the pot, as of the last `sync`: `amount` less what's warming.
    pub counted: u64,
    /// GRAVI on its way out, withdrawable from `unstake_at`.
    pub unstaking: u64,
    pub unstake_at: i64,
    pub index_paid: u128,
    /// SOL earned and not yet claimed, in lamports.
    pub owed: u64,
    /// The day (unix time / DAY_SECS) of `free_used`: baskets made with no burn that day.
    pub free_day: i64,
    pub free_used: u16,
}

impl Stake {
    /// What's warmed up at `now`, whether or not a `sync` has booked it yet.
    pub fn warm(&self, now: i64) -> u64 {
        if self.pending > 0 && now < self.pending_at + STAKE_WARMUP_SECS { self.amount.saturating_sub(self.pending) } else { self.amount }
    }

    /// Baskets a day this stake makes with no burn: one per whole threshold warmed up.
    pub fn free_per_day(&self, threshold: u64, now: i64) -> u64 {
        if threshold == 0 { 0 } else { self.warm(now) / threshold }
    }

    /// Uses one of today's free baskets if one is left.
    pub fn take_free(&mut self, threshold: u64, now: i64) -> bool {
        let day = now.div_euclid(DAY_SECS);
        if self.free_day != day {
            self.free_day = day;
            self.free_used = 0;
        }
        if (self.free_used as u64) < self.free_per_day(threshold, now) {
            self.free_used += 1;
            return true;
        }
        false
    }

    /// Books what it earned up to the pool's index. Call after `pool.accrue`.
    pub fn settle(&mut self, pool: &StakePool) -> Result<()> {
        let earned = pool
            .index
            .saturating_sub(self.index_paid)
            .checked_mul(self.counted as u128)
            .ok_or(VaultError::MathOverflow)?
            / STAKE_INDEX_SCALE;
        self.owed = self.owed.checked_add(u64::try_from(earned).map_err(|_| VaultError::MathOverflow)?).ok_or(VaultError::MathOverflow)?;
        self.index_paid = pool.index;
        Ok(())
    }

    /// Books what has warmed up, and lines up `counted` and the pool's total with it. Call after
    /// `settle`.
    pub fn sync(&mut self, pool: &mut StakePool, now: i64) -> Result<()> {
        self.pending = self.pending.min(self.amount);
        if self.pending > 0 && now >= self.pending_at + STAKE_WARMUP_SECS {
            self.pending = 0;
        }
        let counted = self.amount - self.pending;
        pool.total_counted = pool
            .total_counted
            .checked_sub(self.counted)
            .and_then(|t| t.checked_add(counted))
            .ok_or(VaultError::MathOverflow)?;
        self.counted = counted;
        Ok(())
    }
}

// --------------------------------------------------------------------- share staking, voting

/// Owner of every basket's staked shares, one canonical token account per share mint. It's not the
/// basket, whose stray tokens the keeper may sweep; only a staker's own withdrawal moves these.
pub const SHARE_ESCROW_SEED: &[u8] = b"share_escrow";
pub const SHARE_STAKE_SEED: &[u8] = b"share_stake";
pub const COMMUNITY_SEED: &[u8] = b"community";
pub const PROPOSAL_SEED: &[u8] = b"proposal";
pub const VOTE_SEED: &[u8] = b"vote";
/// How long staked shares take to reach full voting weight.
pub const VOTE_RAMP_SECS: i64 = 3 * 86_400;
/// (The default of the `VOTE_PERIOD_SECS` param: `params.rs`.)
pub const VOTE_PERIOD_SECS: i64 = 3 * 86_400;
/// The wait between a vote passing and its change, so holders who disagree can redeem first.
/// (The default of the `VOTE_TIMELOCK_SECS` param: `params.rs`.)
pub const VOTE_TIMELOCK_SECS: i64 = 86_400;
/// How long a passed proposal stays executable before it lapses.
/// (The default of the `VOTE_GRACE_SECS` param: `params.rs`.)
pub const VOTE_GRACE_SECS: i64 = 7 * 86_400;
/// (These four: the defaults of the params of the same names, `params.rs`.)
pub const PROPOSE_MIN_BPS: u128 = 100;
pub const QUORUM_BPS: u128 = 1_000;
/// Fee, Go Smart and leaving need this much of For + Against; the rest need over half.
pub const SUPERMAJORITY_BPS: u128 = 6_600;
pub const MAX_OPEN_PROPOSALS: u8 = 3;
/// A wallet's open proposal on a basket, at `[OPEN_SEED, basket, proposer]`: one at a time each.
pub const OPEN_SEED: &[u8] = b"open";
/// An asset a vote added can't be removed for this long: room for the Reweight that weights it to
/// be proposed, voted on, and run.
pub const REMOVE_GRACE_SECS: i64 = VOTE_PERIOD_SECS + VOTE_TIMELOCK_SECS + VOTE_GRACE_SECS;

/// A wallet's staked shares of one basket, at `[SHARE_STAKE_SEED, basket, wallet]`. Any basket's
/// holders may stake; votes count only in community baskets.
#[account]
#[derive(InitSpace)]
pub struct ShareStake {
    pub bump: u8,
    pub basket: Pubkey,
    pub wallet: Pubkey,
    pub amount: u64,
    /// Where its weight's ramp started. A top-up moves it forward, by the shares' share.
    pub since: i64,
    /// Shares that voted can't be withdrawn until the vote ends.
    pub locked_until: i64,
}

impl ShareStake {
    /// Voting weight at `at`: rises linearly to `amount` over VOTE_RAMP_SECS. Never more than the
    /// shares staked at `at` had: a later top-up moves `since` so that it only lowers this.
    pub fn power_at(&self, at: i64) -> Result<u64> {
        let held = (at - self.since).clamp(0, VOTE_RAMP_SECS);
        let power = (self.amount as u128).checked_mul(held as u128).ok_or(VaultError::MathOverflow)? / VOTE_RAMP_SECS as u128;
        Ok(power as u64)
    }

    /// Adds `more` shares at `now`, keeping today's weight: `since` first catches up to the end of
    /// the ramp, or new shares would inherit age that old ones earned long ago.
    pub fn add(&mut self, more: u64, now: i64) -> Result<()> {
        let since = self.since.max(now - VOTE_RAMP_SECS);
        let total = self.amount.checked_add(more).ok_or(VaultError::MathOverflow)?;
        let held = (now - since) as u128 * self.amount as u128 / total as u128;
        self.since = now - held as i64;
        self.amount = total;
        Ok(())
    }
}

/// A community basket's voting state, at `[COMMUNITY_SEED, basket]`.
#[account]
#[derive(InitSpace)]
pub struct Community {
    pub bump: u8,
    pub basket: Pubkey,
    /// Proposal ids count up from here, for good, so an address is never reused.
    pub next_id: u32,
    pub open: u8,
    /// Goes up each time the basket leaves Community, which voids what was passed before.
    pub epoch: u32,
    pub hour: i64,
    pub turnover: [u16; TURNOVER_HOURS],
    pub changes: u32,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum ProposalAction {
    /// Back to creator control. First, so a new proposal account, all zeros, reads as a valid one
    /// whatever size the real action gave it.
    Leave,
    /// New targets, one per asset; `mints_hash` pins which assets they mean.
    Reweight { target_bps: Vec<u16>, mints_hash: [u8; 32] },
    AddAsset { mint: Pubkey },
    Policy { rebalance_mode: RebalanceMode, interval_secs: i64, drift_bps: u16 },
    Stops { stops: Vec<crate::instructions::stops::StopArgs> },
    /// Lower only.
    CreatorFee { mgmt_fee_bps: u16 },
    /// Lower only.
    CreatorCut { bps: u16 },
    Split { numerator: u16, denominator: u16 },
    /// Hands the basket to the strategist. The vote holds the creator's Smart powers from then on.
    GoSmart { mandate_hash: [u8; 32], guard: Guardrails },
    /// A new mandate, on a basket a vote handed to the strategist.
    Mandate { mandate_hash: [u8; 32], guard: Guardrails },
}

impl ProposalAction {
    pub fn needs_supermajority(&self) -> bool {
        matches!(self, Self::CreatorFee { .. } | Self::CreatorCut { .. } | Self::GoSmart { .. } | Self::Leave)
    }
}

/// One proposal, at `[PROPOSAL_SEED, basket, id]`. Its proposer pays the rent and gets it back at
/// `community_close`.
#[account]
pub struct Proposal {
    pub bump: u8,
    pub basket: Pubkey,
    pub id: u32,
    pub epoch: u32,
    pub proposer: Pubkey,
    /// Weight is read as of this moment, and quorum from the supply then.
    pub created_at: i64,
    pub supply: u64,
    pub votes_for: u64,
    pub votes_against: u64,
    pub executed: bool,
    pub action: ProposalAction,
}

impl Proposal {
    /// Everything before `action`, discriminator included.
    pub const BASE_SPACE: usize = 8 + 1 + 32 + 4 + 4 + 32 + 8 + 8 + 8 + 8 + 1;

    /// Where its terms sit: right after its action.
    fn terms_at(&self) -> Result<usize> {
        Ok(Self::BASE_SPACE + self.action.try_to_vec()?.len())
    }

    /// Its windows and vote rules: those written on it when it opened (`write_terms`), or, for a
    /// proposal opened before they were written, the constants then. A later change to the params
    /// never moves a vote already open.
    pub fn terms(&self, ai: &AccountInfo) -> Result<Terms> {
        let at = self.terms_at()?;
        let d = ai.try_borrow_data()?;
        if let Some(t) = d.get(at..at + TERMS_LEN).filter(|t| t[0] == TERMS_VERSION) {
            let word = |k: usize| i64::from_le_bytes(t[k..k + 8].try_into().unwrap());
            let half = |k: usize| u16::from_le_bytes([t[k], t[k + 1]]) as u128;
            return Ok(Terms { vote_end: word(1), exec_after: word(9), exec_until: word(17), quorum_bps: half(25), super_bps: half(27) });
        }
        let vote_end = self.created_at + VOTE_PERIOD_SECS;
        Ok(Terms {
            vote_end,
            exec_after: vote_end + VOTE_TIMELOCK_SECS,
            exec_until: vote_end + VOTE_TIMELOCK_SECS + VOTE_GRACE_SECS,
            quorum_bps: QUORUM_BPS,
            super_bps: SUPERMAJORITY_BPS,
        })
    }

    /// Writes its terms after its action; the account was sized with `TERMS_LEN` for them.
    pub fn write_terms(&self, ai: &AccountInfo, t: &Terms) -> Result<()> {
        let at = self.terms_at()?;
        let mut d = ai.try_borrow_mut_data()?;
        let out = d.get_mut(at..at + TERMS_LEN).ok_or(VaultError::BadProposal)?;
        out[0] = TERMS_VERSION;
        out[1..9].copy_from_slice(&t.vote_end.to_le_bytes());
        out[9..17].copy_from_slice(&t.exec_after.to_le_bytes());
        out[17..25].copy_from_slice(&t.exec_until.to_le_bytes());
        out[25..27].copy_from_slice(&(t.quorum_bps as u16).to_le_bytes());
        out[27..29].copy_from_slice(&(t.super_bps as u16).to_le_bytes());
        Ok(())
    }

    /// Enough of the supply voted, and enough of them for it. Final once voting has ended.
    pub fn passed(&self, t: &Terms) -> bool {
        let (yes, no) = (self.votes_for as u128, self.votes_against as u128);
        let total = yes + no;
        if total * BPS_DENOM < self.supply as u128 * t.quorum_bps {
            return false;
        }
        if self.action.needs_supermajority() {
            yes * BPS_DENOM >= total * t.super_bps
        } else {
            yes > no
        }
    }
}

/// A proposal's windows and vote rules, fixed when it opens: `[version 1][vote_end i64][exec_after
/// i64][exec_until i64][quorum_bps u16][supermajority_bps u16]`, after its action.
pub const TERMS_VERSION: u8 = 1;
pub const TERMS_LEN: usize = 29;

#[derive(Clone, Copy)]
pub struct Terms {
    pub vote_end: i64,
    pub exec_after: i64,
    pub exec_until: i64,
    pub quorum_bps: u128,
    pub super_bps: u128,
}

/// That proposal `id` is its proposer's one open proposal on `basket`, at `[OPEN_SEED, basket,
/// proposer]`. The proposer pays its rent at `community_propose` and gets it back at
/// `community_close`. Proposals opened before it existed have none.
#[account]
#[derive(InitSpace)]
pub struct OpenProposal {
    pub bump: u8,
    pub basket: Pubkey,
    pub id: u32,
}

/// One wallet's vote, at `[VOTE_SEED, proposal, wallet]`: that it exists is what stops a second.
#[account]
#[derive(InitSpace)]
pub struct VoteRecord {
    pub bump: u8,
    pub wallet: Pubkey,
    pub vote_end: i64,
    pub support: bool,
}
