//! The protocol's tunables, at `[PARAMS_SEED]`: values that used to be constants in code, which the
//! protocol authority moves with `set_param`, each inside bounds written here. A value never set
//! (0), or the account not existing at all, reads as the constant it replaced, so the program
//! behaves exactly as before until a value is set.
//!
//! What never moves without an upgrade: the fee and burn ceilings and floors (`MAX_PROTOCOL_FEE_BPS`,
//! `MAX_SMART_FEE_BPS`, `MIN_BURN_SHARE_BPS`, `MAX_CREATOR_FEE_BPS`, `MAX_CREATOR_CUT_BPS`, the burn
//! ceilings), the bounds below, and the notice itself.
//!
//! How a change applies:
//! - Operational values (price checks and freshness, keeper and buyback limits, the invite length)
//!   apply at once: they bound the app's own keys and spend no holder's money beyond the bounds.
//! - Values holders and creators live by (stop rules, policy minimums, the transfer-fee cap, split
//!   cooldown, notices, Smart ceilings, staking and vote rules) wait `PARAM_NOTICE_SECS` (a week, as
//!   a staking-threshold raise does), so anyone who disagrees can leave first.
//! - Rebalance slippage and the slice caps protect holders in one direction only: lowering them
//!   applies at once, raising them waits the notice.
//! - Kill switches (`KILL`) apply at once either way: they only stop things, and lifting one returns
//!   to the rules as documented. Redeeming, unstaking and every other way out are behind none.
//! - A new feature shipped dark (`FEATURES`) waits the notice to be turned on, and turns off at once.
//!
//! Outside this account, by the same rule: a fee or burn raise (`set_param` CONFIG_*) waits a week
//! in the protocol account's tail (`state::FEE_NOTICE_SECS`), lowering is at once; a key named for a
//! role (`set_role`) waits a day (`state::ROLE_NOTICE_SECS`), removing one is at once.
//!
//! Durations are read when a window opens and stored on the account it belongs to (a proposal's
//! vote and execution window, a stake's warm-up, a cut rise, a mandate, an unstaking), so a change
//! never moves a window already open.

use anchor_lang::prelude::*;

use crate::errors::VaultError;
use crate::state::{self as st, *};

pub const PARAMS_SEED: &[u8] = b"params";
/// `[PARAMS_SEED]` under the program's id (tests/params.test.ts checks it).
pub const PARAMS_ID: Pubkey = pubkey!("5atmoVAHht1bK7MsEogYY2e8xZgYMGT9G2nVBPeTebr1");
pub const PARAMS_BUMP: u8 = 255;
/// sha256("account:Params")[..8], as Anchor would name it.
pub const PARAMS_DISCRIMINATOR: [u8; 8] = [129, 232, 120, 183, 156, 198, 9, 242];
pub const PARAMS_VERSION: u8 = 1;
/// [discriminator 8][version 1][bump 1][spare 6], then one slot per id.
const HEADER: usize = 16;
/// [value u64][pending u64][pending_at i64], little-endian. `pending` applies from `pending_at`
/// (0: nothing pending); a value of 0 means unset, the default.
const SLOT_LEN: usize = 24;
/// Room for 80 ids; 41 are used, the rest (about 0.9 KB) wait for future values.
pub const PARAM_SLOTS: usize = 80;
pub const PARAMS_LEN: usize = HEADER + PARAM_SLOTS * SLOT_LEN;
/// How long a change holders live by waits before it applies.
pub const PARAM_NOTICE_SECS: i64 = 7 * 86_400;

// ------------------------------------------------------------------------------------- ids

/// Kill switches: a bit per family below; set, that family's instructions refuse.
pub const KILL: u16 = 0;
/// Features shipped dark, a bit each (none yet: `KNOWN_FEATURES`).
pub const FEATURES: u16 = 1;
pub const REBALANCE_SLIPPAGE_BPS: u16 = 2;
pub const BUYBACK_SLIPPAGE_BPS: u16 = 3;
pub const SLICE_BPS: u16 = 4;
pub const LEAVING_SLICE_BPS: u16 = 5;
pub const KEEPER_REFUND_E12: u16 = 6;
pub const BUYBACK_REFUND_E12: u16 = 7;
pub const PRICE_MOVE_BPS: u16 = 8;
pub const PRICE_REFERENCE_SECS: u16 = 9;
pub const PYTH_AGE_SECS: u16 = 10;
pub const PYTH_TOLERANCE_MIN_BPS: u16 = 11;
pub const PYTH_TOLERANCE_MAX_BPS: u16 = 12;
pub const PYTH_CONF_MULTIPLE: u16 = 13;
pub const PRICE_AGE_SECS: u16 = 14;
pub const DEAD_PRICE_SECS: u16 = 15;
pub const STOP_CONFIRM_SECS: u16 = 16;
pub const STOP_WINDOW_SECS: u16 = 17;
pub const STOP_MIN_BPS: u16 = 18;
pub const STOP_MAX_BPS: u16 = 19;
pub const POLICY_MIN_DRIFT_BPS: u16 = 20;
pub const POLICY_MIN_INTERVAL_SECS: u16 = 21;
pub const TRANSFER_FEE_CAP_BPS: u16 = 22;
pub const SPLIT_COOLDOWN_SECS: u16 = 23;
pub const CUT_NOTICE_SECS: u16 = 24;
pub const INVITE_MAX_SECS: u16 = 25;
pub const SMART_MAX_DAY_BPS: u16 = 26;
pub const SMART_MAX_WEEK_BPS: u16 = 27;
pub const SMART_MAX_WEIGHT_BPS: u16 = 28;
pub const SMART_MAX_UNVERIFIED_BPS: u16 = 29;
pub const MANDATE_NOTICE_SECS: u16 = 30;
pub const STAKE_WARMUP_SECS: u16 = 31;
pub const STAKE_UNSTAKE_SECS: u16 = 32;
pub const STAKE_PERIOD_SECS: u16 = 33;
pub const VOTE_PERIOD_SECS: u16 = 34;
pub const VOTE_TIMELOCK_SECS: u16 = 35;
pub const VOTE_GRACE_SECS: u16 = 36;
pub const PROPOSE_MIN_BPS: u16 = 37;
pub const QUORUM_BPS: u16 = 38;
pub const SUPERMAJORITY_BPS: u16 = 39;
pub const MAX_OPEN_PROPOSALS: u16 = 40;

// ------------------------------------------------------------------------- kill switches

pub const FAMILY_MINT: u64 = 1 << 0;
pub const FAMILY_REBALANCE: u64 = 1 << 1;
pub const FAMILY_BUYBACK: u64 = 1 << 2;
pub const FAMILY_STOPS: u64 = 1 << 3;
pub const FAMILY_SMART: u64 = 1 << 4;
pub const FAMILY_STAKING: u64 = 1 << 5;
pub const FAMILY_COMMUNITY: u64 = 1 << 6;
pub const FAMILY_INVITES: u64 = 1 << 7;
pub const FAMILY_LISTING: u64 = 1 << 8;
pub const FAMILY_CREATE: u64 = 1 << 9;
pub const ALL_FAMILIES: u64 = (1 << 10) - 1;
/// Dark features this build knows. A bit outside it can't be set: a later upgrade that gives a bit
/// meaning must find it off, and turning it on then waits the notice.
pub const KNOWN_FEATURES: u64 = 0;

// ---------------------------------------------------------------------------------- bounds

/// Applies at once.
const NOW: u8 = 0;
/// Waits the notice.
const WAIT: u8 = 1;
/// Lower at once, higher after the notice.
const UP_WAITS: u8 = 2;
/// Lowering waits the notice (a shorter window is a looser one); raising applies at once.
const DOWN_WAITS: u8 = 3;

const DAY: u64 = 86_400;

/// `[default, min, max, how it applies]` per id. The defaults are the constants these replaced.
/// Why each bound is where it is:
/// - slippage: a rebalance 0.1%-5% (today 3%: a memecoin two pools from SOL costs ~3.7% in pool fees
///   alone, so 5% leaves room for one more hop, never more); a buyback 0.5%-15% (today 10%; it sells
///   protocol fees, never a holder's deposit).
/// - slices: 0.5%-10% of a basket per slice (today 5%); a clip of a leaving asset 5%-100% (today 50%).
/// - keeper refunds: up to $0.25 a slice (today $0.05), $2 a buyback run (today $0.50): what a
///   leaked keeper key could take per transaction.
/// - prices: a move without Pyth 2%-30% per reference (today 20%), the reference rolling every 15
///   minutes to a day (today an hour), so a leaked price key still needs hours to move a price far;
///   Pyth at most 15 minutes old (today 5), within 0.5%-10% (today 3%-5%) at 1-10× its confidence;
///   prices used at most 30 s-10 min old (today 2 min); a 0% sleeve unpriced for 1 h-30 days counts
///   as nothing (today a day).
/// - stops: confirmation 1-60 minutes (today 5), the arming window 2 minutes-3 hours (today 15); a
///   stop 1%-20% at the closest (today 5%), 50%-95% at the furthest (today 90%).
/// - policy: drift at least 0.1%-5% (today 0.5%), a schedule at least hourly to weekly (today daily).
/// - transfer-fee cap 1%-8% (today 4%); split cooldown 1-90 days (today 30); a cut rise's notice 6
///   hours to 30 days (today a day); an invite up to 1-93 days (today 31).
/// - Smart ceilings: 5%-60% a day (today 40%), 10%-150% a week (today 100%), one asset 10%-60%
///   (today 35%), unverified 1%-50% (today 30%); a mandate's notice 6 hours to 14 days (today 1).
/// - staking: warm-up 1 hour to 14 days (today 2), unstaking 1-30 days (today 7), a pot's month
///   7-90 days (today 30).
/// - votes: voting 1-14 days (today 3), the wait after 1 hour-7 days (today 1), the window to run
///   it 1-30 days (today 7); proposing takes 0.1%-10% of the supply (today 1%), quorum 1%-50%
///   (today 10%), the supermajority 51%-90% (today 66%), 1-10 open proposals (today 3).
const SPECS: [(u64, u64, u64, u8); 41] = [
    (0, 0, 0, NOW),                                        // KILL (checked by hand)
    (0, 0, 0, WAIT),                                       // FEATURES (checked by hand)
    (MAX_REBALANCE_SLIPPAGE_BPS as u64, 10, 500, UP_WAITS),
    (MAX_BUYBACK_SLIPPAGE_BPS as u64, 50, 1_500, UP_WAITS),
    (MAX_SLICE_BPS as u64, 50, 1_000, UP_WAITS),
    (MAX_LEAVING_SLICE_BPS as u64, 500, 10_000, UP_WAITS),
    (MAX_KEEPER_REFUND_E12 as u64, 1, 250_000_000_000, UP_WAITS),
    (MAX_BUYBACK_REFUND_E12 as u64, 1, 2_000_000_000_000, UP_WAITS),
    (MAX_PRICE_MOVE_BPS as u64, 200, 3_000, UP_WAITS),
    (st::PRICE_REFERENCE_SECS as u64, 900, DAY, DOWN_WAITS),
    (MAX_PYTH_AGE_SECS as u64, 30, 900, UP_WAITS),
    (st::PYTH_TOLERANCE_MIN_BPS as u64, 50, 1_000, UP_WAITS),
    (st::PYTH_TOLERANCE_MAX_BPS as u64, 100, 1_000, UP_WAITS),
    (st::PYTH_CONF_MULTIPLE as u64, 1, 10, UP_WAITS),
    (MAX_PRICE_AGE_SECS as u64, 30, 600, UP_WAITS),
    (st::DEAD_PRICE_SECS as u64, 3_600, 30 * DAY, WAIT),
    (st::STOP_CONFIRM_SECS as u64, 60, 3_600, WAIT),
    (STOP_ARM_WINDOW_SECS as u64, 120, 10_800, WAIT),
    (MIN_STOP_BPS as u64, 100, 2_000, WAIT),
    (MAX_STOP_BPS as u64, 5_000, 9_500, WAIT),
    (MIN_POLICY_DRIFT_BPS as u64, 10, 500, WAIT),
    (MIN_POLICY_INTERVAL_SECS as u64, 3_600, 7 * DAY, WAIT),
    (MAX_TRANSFER_FEE_BPS as u64, 100, 800, WAIT),
    (st::SPLIT_COOLDOWN_SECS as u64, DAY, 90 * DAY, WAIT),
    (st::CUT_NOTICE_SECS as u64, 6 * 3_600, 30 * DAY, WAIT),
    (MAX_INVITE_SECS as u64, DAY, 93 * DAY, NOW),
    (st::SMART_MAX_DAY_BPS as u64, 500, 6_000, WAIT),
    (st::SMART_MAX_WEEK_BPS as u64, 1_000, 15_000, WAIT),
    (st::SMART_MAX_WEIGHT_BPS as u64, 1_000, 6_000, WAIT),
    (st::SMART_MAX_UNVERIFIED_BPS as u64, 100, 5_000, WAIT),
    (st::MANDATE_NOTICE_SECS as u64, 6 * 3_600, 14 * DAY, WAIT),
    (st::STAKE_WARMUP_SECS as u64, 3_600, 14 * DAY, WAIT),
    (st::STAKE_UNSTAKE_SECS as u64, DAY, 30 * DAY, WAIT),
    (st::STAKE_PERIOD_SECS as u64, 7 * DAY, 90 * DAY, WAIT),
    (st::VOTE_PERIOD_SECS as u64, DAY, 14 * DAY, WAIT),
    (st::VOTE_TIMELOCK_SECS as u64, 3_600, 7 * DAY, WAIT),
    (st::VOTE_GRACE_SECS as u64, DAY, 30 * DAY, WAIT),
    (st::PROPOSE_MIN_BPS as u64, 10, 1_000, WAIT),
    (st::QUORUM_BPS as u64, 100, 5_000, WAIT),
    (st::SUPERMAJORITY_BPS as u64, 5_100, 9_000, WAIT),
    (st::MAX_OPEN_PROPOSALS as u64, 1, 10, WAIT),
];

// ---------------------------------------------------------------------------------- reading

/// The tunables in force, read once per instruction from the `params` account it names (pinned to
/// `PARAMS_ID` by its Accounts struct, so it can't be left out or swapped). Not created yet: every
/// value is its default and nothing is switched off.
pub struct Params {
    data: Vec<u8>,
    pub now: i64,
}

impl Params {
    pub fn load(ai: &AccountInfo) -> Result<Self> {
        let now = Clock::get()?.unix_timestamp;
        require_keys_eq!(*ai.key, PARAMS_ID, VaultError::BadParams);
        if ai.owner != &crate::ID {
            return Ok(Self { data: Vec::new(), now });
        }
        let d = ai.try_borrow_data()?;
        require!(d.len() >= PARAMS_LEN && d[..8] == PARAMS_DISCRIMINATOR, VaultError::BadParams);
        Ok(Self { data: d[..PARAMS_LEN].to_vec(), now })
    }

    /// What's in force for `id`, 0 if unset: a pending value once its notice has run out.
    pub fn raw(&self, id: u16) -> u64 {
        if self.data.is_empty() {
            return 0;
        }
        let (value, pending, at) = read_slot(&self.data, id);
        if at != 0 && self.now >= at { pending } else { value }
    }

    /// The value in force for `id`, its default if unset.
    pub fn get(&self, id: u16) -> u64 {
        match self.raw(id) {
            0 => SPECS[id as usize].0,
            v => v,
        }
    }

    pub fn secs(&self, id: u16) -> i64 {
        self.get(id) as i64
    }

    pub fn bps(&self, id: u16) -> u16 {
        self.get(id) as u16
    }

    pub fn wide(&self, id: u16) -> u128 {
        self.get(id) as u128
    }

    /// Refuses if `family`'s kill switch is on.
    pub fn require_on(&self, family: u64) -> Result<()> {
        require!(self.raw(KILL) & family == 0, VaultError::FeatureOff);
        Ok(())
    }

    pub fn feature_on(&self, bit: u64) -> bool {
        self.raw(FEATURES) & bit != 0
    }

    /// How long after arming a stop can still fire: above the confirmation, three times it if not.
    pub fn stop_window(&self) -> i64 {
        let (confirm, window) = (self.secs(STOP_CONFIRM_SECS), self.secs(STOP_WINDOW_SECS));
        if window > confirm { window } else { 3 * confirm }
    }

    /// The stop range, closest first; a closest set above the furthest reads as the furthest.
    pub fn stop_range(&self) -> (u16, u16) {
        let (lo, hi) = (self.bps(STOP_MIN_BPS), self.bps(STOP_MAX_BPS));
        (lo.min(hi), hi)
    }

    /// Smart ceilings `(day, week, weight, unverified)`; unverified never above the weight cap.
    pub fn smart_caps(&self) -> (u16, u16, u16, u16) {
        let weight = self.bps(SMART_MAX_WEIGHT_BPS);
        (self.bps(SMART_MAX_DAY_BPS), self.bps(SMART_MAX_WEEK_BPS), weight, self.bps(SMART_MAX_UNVERIFIED_BPS).min(weight))
    }

    /// An asset a vote added can't be removed for this long: room for its Reweight to be proposed,
    /// voted on, and run.
    pub fn remove_grace(&self) -> i64 {
        self.secs(VOTE_PERIOD_SECS) + self.secs(VOTE_TIMELOCK_SECS) + self.secs(VOTE_GRACE_SECS)
    }
}

fn read_slot(data: &[u8], id: u16) -> (u64, u64, i64) {
    let o = HEADER + id as usize * SLOT_LEN;
    let word = |k: usize| u64::from_le_bytes(data[o + k..o + k + 8].try_into().unwrap());
    (word(0), word(8), word(16) as i64)
}

fn write_slot(data: &mut [u8], id: u16, value: u64, pending: u64, at: i64) {
    let o = HEADER + id as usize * SLOT_LEN;
    data[o..o + 8].copy_from_slice(&value.to_le_bytes());
    data[o + 8..o + 16].copy_from_slice(&pending.to_le_bytes());
    data[o + 16..o + 24].copy_from_slice(&at.to_le_bytes());
}

// ---------------------------------------------------------------------------------- writing

/// Writes `id` = `value` (0: back to the default) into the params account's bytes, within its
/// bounds, at once or after the notice as its rule says. Proposing the value already in force
/// withdraws a pending change.
pub(crate) fn write(data: &mut [u8], id: u16, value: u64, now: i64) -> Result<()> {
    require!((id as usize) < SPECS.len(), VaultError::UnknownParam);
    let (default, min, max, rule) = SPECS[id as usize];
    // A change whose notice has run out is the value now.
    let (mut current, pending, at) = read_slot(data, id);
    if at != 0 && now >= at {
        current = pending;
    }
    let instant = match id {
        KILL => {
            require!(value & !ALL_FAMILIES == 0, VaultError::ParamOutOfBounds);
            true
        }
        FEATURES => {
            require!(value & !KNOWN_FEATURES == 0, VaultError::ParamOutOfBounds);
            // Turning one off, or leaving them as they are, applies at once; turning one on waits.
            value & !current == 0
        }
        _ => {
            require!(value == 0 || (min..=max).contains(&value), VaultError::ParamOutOfBounds);
            let (new, old) = (if value == 0 { default } else { value }, if current == 0 { default } else { current });
            match rule {
                NOW => true,
                UP_WAITS => new <= old,
                DOWN_WAITS => new >= old,
                _ => new == old,
            }
        }
    };
    if instant {
        write_slot(data, id, value, 0, 0);
    } else {
        write_slot(data, id, current, value, now + PARAM_NOTICE_SECS);
    }
    Ok(())
}

/// Creates the params account the first time a value is set, the authority paying its rent.
pub(crate) fn ensure<'info>(params: &AccountInfo<'info>, payer: &AccountInfo<'info>, system_program: &AccountInfo<'info>) -> Result<()> {
    if params.owner == &crate::ID {
        return Ok(());
    }
    crate::token_util::create_pda(payer, params, system_program, &[PARAMS_SEED, &[PARAMS_BUMP]], PARAMS_LEN)?;
    let mut d = params.try_borrow_mut_data()?;
    d[..8].copy_from_slice(&PARAMS_DISCRIMINATOR);
    d[8] = PARAMS_VERSION;
    d[9] = PARAMS_BUMP;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Vec<u8> {
        vec![0u8; PARAMS_LEN]
    }

    fn view(data: &[u8], now: i64) -> Params {
        Params { data: data.to_vec(), now }
    }

    #[test]
    fn defaults_are_the_old_constants() {
        let p = Params { data: Vec::new(), now: 0 };
        assert_eq!(p.bps(REBALANCE_SLIPPAGE_BPS), MAX_REBALANCE_SLIPPAGE_BPS);
        assert_eq!(p.secs(VOTE_PERIOD_SECS), st::VOTE_PERIOD_SECS);
        assert_eq!(p.stop_window(), STOP_ARM_WINDOW_SECS);
        assert_eq!(p.remove_grace(), REMOVE_GRACE_SECS);
        assert!(p.require_on(ALL_FAMILIES).is_ok());
    }

    #[test]
    fn notice_rules() {
        let mut d = fresh();
        // Lowering slippage is at once, raising waits.
        write(&mut d, REBALANCE_SLIPPAGE_BPS, 200, 100).unwrap();
        assert_eq!(view(&d, 100).bps(REBALANCE_SLIPPAGE_BPS), 200);
        write(&mut d, REBALANCE_SLIPPAGE_BPS, 400, 100).unwrap();
        assert_eq!(view(&d, 100 + PARAM_NOTICE_SECS - 1).bps(REBALANCE_SLIPPAGE_BPS), 200);
        assert_eq!(view(&d, 100 + PARAM_NOTICE_SECS).bps(REBALANCE_SLIPPAGE_BPS), 400);
        // Out of bounds.
        assert!(write(&mut d, REBALANCE_SLIPPAGE_BPS, 501, 100).is_err());
        // A vote rule waits either way; the value in force withdraws it.
        write(&mut d, QUORUM_BPS, 500, 0).unwrap();
        assert_eq!(view(&d, 1).bps(QUORUM_BPS), 1_000);
        write(&mut d, QUORUM_BPS, 1_000, 1).unwrap();
        assert_eq!(view(&d, PARAM_NOTICE_SECS * 2).bps(QUORUM_BPS), 1_000);
        // Features: on waits (none known yet, so refused); kill switches at once.
        assert!(write(&mut d, FEATURES, 1, 0).is_err());
        write(&mut d, KILL, FAMILY_MINT, 0).unwrap();
        assert!(view(&d, 0).require_on(FAMILY_MINT).is_err());
        assert!(write(&mut d, KILL, 1 << 20, 0).is_err());
    }
}
