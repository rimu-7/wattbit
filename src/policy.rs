//! Charge-limit policy: validated thresholds, the hysteresis state machine
//! and the timing constants the daemon evaluates it with.
//!
//! With an upper limit `U` and lower limit `L`:
//! - charge >= U: [`ChargeState::ChargingSuppressed`]
//! - L <= charge < U after having been suppressed:
//!   [`ChargeState::WaitingToResumeCharging`] (still no charging)
//! - charge < L: [`ChargeState::ChargingPermitted`], which persists until
//!   the charge reaches U again.

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const MIN_UPPER: u8 = 50;
pub const MAX_UPPER: u8 = 100;
pub const DEFAULT_UPPER: u8 = 80;
/// Width of the default deadband between the upper and lower limits.
pub const DEFAULT_HYSTERESIS: u8 = 5;

/// When the adapter is bypassed and the charge falls to this level, the
/// daemon re-enables the adapter regardless of configuration.
pub const ADAPTER_SAFETY_FLOOR: u8 = 10;

/// How often the daemon re-reads telemetry and re-applies the policy.
pub const EVALUATION_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("upper limit must be between {MIN_UPPER}% and {MAX_UPPER}%, got {0}%")]
    UpperOutOfRange(u8),
    #[error("lower limit ({lower}%) must be below the upper limit ({upper}%)")]
    LowerNotBelowUpper { lower: u8, upper: u8 },
}

/// A validated pair of charge thresholds. The invariants
/// `MIN_UPPER <= upper <= MAX_UPPER` and `lower < upper` hold for every
/// value, including deserialized ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawThresholds", into = "RawThresholds")]
pub struct Thresholds {
    upper: u8,
    lower: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawThresholds {
    upper: u8,
    lower: u8,
}

impl TryFrom<RawThresholds> for Thresholds {
    type Error = PolicyError;

    fn try_from(raw: RawThresholds) -> Result<Self, Self::Error> {
        Thresholds::new(raw.upper, Some(raw.lower))
    }
}

impl From<Thresholds> for RawThresholds {
    fn from(t: Thresholds) -> Self {
        RawThresholds {
            upper: t.upper,
            lower: t.lower,
        }
    }
}

impl Thresholds {
    /// Builds thresholds from an upper limit and an optional explicit lower
    /// limit. Without one, the lower limit is `max(0, upper - 5)`.
    pub fn new(upper: u8, lower: Option<u8>) -> Result<Self, PolicyError> {
        if !(MIN_UPPER..=MAX_UPPER).contains(&upper) {
            return Err(PolicyError::UpperOutOfRange(upper));
        }
        let lower = lower.unwrap_or_else(|| Self::default_lower(upper));
        if lower >= upper {
            return Err(PolicyError::LowerNotBelowUpper { lower, upper });
        }
        Ok(Self { upper, lower })
    }

    pub fn default_lower(upper: u8) -> u8 {
        upper.saturating_sub(DEFAULT_HYSTERESIS)
    }

    pub fn upper(self) -> u8 {
        self.upper
    }

    pub fn lower(self) -> u8 {
        self.lower
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            upper: DEFAULT_UPPER,
            lower: DEFAULT_UPPER - DEFAULT_HYSTERESIS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeState {
    /// Charge reached the upper limit; charging is held off.
    ChargingSuppressed,
    /// Discharging through the deadband between the limits.
    WaitingToResumeCharging,
    /// Charge fell below the lower limit; charging runs until the upper limit.
    ChargingPermitted,
}

impl ChargeState {
    pub fn allows_charging(self) -> bool {
        self == ChargeState::ChargingPermitted
    }
}

/// One step of the hysteresis state machine.
///
/// `previous` is `None` when no state has been established yet (daemon
/// start, or thresholds just changed). A reading inside the deadband then
/// permits charging up to the upper limit, so newly set limits are reached
/// rather than left short.
pub fn next_state(previous: Option<ChargeState>, charge: u8, thresholds: Thresholds) -> ChargeState {
    if charge >= thresholds.upper {
        ChargeState::ChargingSuppressed
    } else if charge < thresholds.lower {
        ChargeState::ChargingPermitted
    } else {
        match previous {
            None | Some(ChargeState::ChargingPermitted) => ChargeState::ChargingPermitted,
            Some(ChargeState::ChargingSuppressed | ChargeState::WaitingToResumeCharging) => {
                ChargeState::WaitingToResumeCharging
            }
        }
    }
}

/// Holds the configured thresholds and the current state of the hysteresis
/// machine.
#[derive(Debug, Clone, Default)]
pub struct ChargeLimiter {
    thresholds: Option<Thresholds>,
    state: Option<ChargeState>,
}

impl ChargeLimiter {
    pub fn new(thresholds: Option<Thresholds>) -> Self {
        Self {
            thresholds,
            state: None,
        }
    }

    pub fn thresholds(&self) -> Option<Thresholds> {
        self.thresholds
    }

    /// `None` while no limit is configured or before the first reading.
    pub fn state(&self) -> Option<ChargeState> {
        self.state
    }

    /// Replaces the thresholds. Changing them restarts the state machine.
    pub fn set_thresholds(&mut self, thresholds: Option<Thresholds>) {
        if self.thresholds != thresholds {
            self.thresholds = thresholds;
            self.state = None;
        }
    }

    /// Feeds a charge reading. Returns the new state, or `None` when no
    /// limit is configured (factory behaviour: charging allowed).
    pub fn update(&mut self, charge: u8) -> Option<ChargeState> {
        let thresholds = self.thresholds?;
        let state = next_state(self.state, charge.min(100), thresholds);
        self.state = Some(state);
        Some(state)
    }

    /// Whether the policy currently allows charging.
    pub fn allows_charging(&self) -> bool {
        self.state.is_none_or(ChargeState::allows_charging)
    }
}

/// Whether a bypassed adapter must be re-enabled to protect the battery.
pub fn adapter_must_be_restored(charge: u8) -> bool {
    charge <= ADAPTER_SAFETY_FLOOR
}
