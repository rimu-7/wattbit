use berry::policy::{
    adapter_must_be_restored, next_state, ChargeLimiter, ChargeState, PolicyError, Thresholds,
    ADAPTER_SAFETY_FLOOR,
};

use ChargeState::*;

fn t(upper: u8, lower: Option<u8>) -> Thresholds {
    Thresholds::new(upper, lower).unwrap()
}

#[test]
fn default_lower_is_five_below_upper() {
    assert_eq!(t(80, None).lower(), 75);
    assert_eq!(t(50, None).lower(), 45);
    assert_eq!(t(100, None).lower(), 95);
    assert_eq!(Thresholds::default(), t(80, None));
}

#[test]
fn default_lower_saturates_at_zero() {
    assert_eq!(Thresholds::default_lower(3), 0);
    assert_eq!(Thresholds::default_lower(5), 0);
}

#[test]
fn upper_limit_range_is_enforced() {
    assert_eq!(
        Thresholds::new(49, None),
        Err(PolicyError::UpperOutOfRange(49))
    );
    assert_eq!(
        Thresholds::new(101, None),
        Err(PolicyError::UpperOutOfRange(101))
    );
    assert!(Thresholds::new(50, None).is_ok());
    assert!(Thresholds::new(100, None).is_ok());
}

#[test]
fn explicit_lower_must_be_below_upper() {
    assert_eq!(t(80, Some(60)).lower(), 60);
    assert_eq!(t(80, Some(0)).lower(), 0);
    assert_eq!(
        Thresholds::new(80, Some(80)),
        Err(PolicyError::LowerNotBelowUpper {
            lower: 80,
            upper: 80
        })
    );
    assert!(Thresholds::new(80, Some(90)).is_err());
}

#[test]
fn transitions_at_boundaries() {
    let th = t(80, None);
    assert_eq!(next_state(None, 80, th), ChargingSuppressed);
    assert_eq!(next_state(None, 100, th), ChargingSuppressed);
    assert_eq!(
        next_state(Some(ChargingSuppressed), 79, th),
        WaitingToResumeCharging
    );
    assert_eq!(
        next_state(Some(WaitingToResumeCharging), 75, th),
        WaitingToResumeCharging
    );
    assert_eq!(
        next_state(Some(WaitingToResumeCharging), 74, th),
        ChargingPermitted
    );
    assert_eq!(
        next_state(Some(ChargingPermitted), 79, th),
        ChargingPermitted
    );
    assert_eq!(
        next_state(Some(ChargingPermitted), 80, th),
        ChargingSuppressed
    );
}

#[test]
fn first_reading_inside_deadband_charges_to_upper() {
    assert_eq!(next_state(None, 77, t(80, None)), ChargingPermitted);
}

#[test]
fn full_hysteresis_cycle() {
    let mut limiter = ChargeLimiter::new(Some(t(80, None)));
    let readings = [70, 75, 79, 80, 81, 79, 76, 75, 74, 76, 79, 80];
    let expected = [
        ChargingPermitted,
        ChargingPermitted,
        ChargingPermitted,
        ChargingSuppressed,
        ChargingSuppressed,
        WaitingToResumeCharging,
        WaitingToResumeCharging,
        WaitingToResumeCharging,
        ChargingPermitted,
        ChargingPermitted,
        ChargingPermitted,
        ChargingSuppressed,
    ];
    for (charge, want) in readings.into_iter().zip(expected) {
        assert_eq!(limiter.update(charge), Some(want), "at {charge}%");
        assert_eq!(limiter.allows_charging(), want.allows_charging());
    }
}

#[test]
fn no_limit_means_charging_allowed() {
    let mut limiter = ChargeLimiter::new(None);
    assert_eq!(limiter.update(100), None);
    assert!(limiter.allows_charging());
}

#[test]
fn changing_thresholds_resets_state() {
    let mut limiter = ChargeLimiter::new(Some(t(80, None)));
    limiter.update(82);
    assert_eq!(limiter.state(), Some(ChargingSuppressed));

    limiter.set_thresholds(Some(t(80, None)));
    assert_eq!(
        limiter.state(),
        Some(ChargingSuppressed),
        "same thresholds keep state"
    );

    limiter.set_thresholds(Some(t(90, None)));
    assert_eq!(limiter.state(), None);
    assert_eq!(limiter.update(82), Some(ChargingPermitted));

    limiter.set_thresholds(None);
    assert_eq!(limiter.state(), None);
    assert!(limiter.allows_charging());
}

#[test]
fn readings_above_100_are_clamped() {
    let mut limiter = ChargeLimiter::new(Some(t(100, None)));
    assert_eq!(limiter.update(255), Some(ChargingSuppressed));
}

#[test]
fn adapter_safety_floor() {
    assert!(adapter_must_be_restored(ADAPTER_SAFETY_FLOOR));
    assert!(adapter_must_be_restored(0));
    assert!(!adapter_must_be_restored(ADAPTER_SAFETY_FLOOR + 1));
}
