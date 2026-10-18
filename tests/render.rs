use wattbit::battery::{BatteryTelemetry, PowerSource};
use wattbit::capabilities::{Capabilities, ChargeControlMode, HostInfo, Support};
use wattbit::client::render_status;
use wattbit::config::Config;
use wattbit::conflicts::Conflict;
use wattbit::policy::{ChargeState, Thresholds};
use wattbit::protocol::{ChargingStatus, StatusReport};

fn capabilities() -> Capabilities {
    Capabilities {
        host: HostInfo {
            arch: "arm64 (Apple Silicon)".into(),
            apple_silicon: true,
            translated: false,
            model: Some("Mac15,6".into()),
            os_version: Some("15.1".into()),
        },
        smc: Support::Supported,
        charge_inhibit: Support::Supported,
        charge_inhibit_control: Some("CH0B/CH0C".into()),
        mcl: Support::unsupported("PowerUI not tested in mock"),
        mcl_available_limits: vec![80, 85, 90, 95, 100],
        firmware_limit: Support::unsupported("SMC key BCLM is not present on this firmware"),
        adapter_control: Support::Supported,
        adapter_control_keys: Some("CH0I".into()),
        magsafe_led: Support::unsupported("SMC key ACLC is not present"),
        sleep_hooks: Support::Supported,
        calibration: Support::unsupported("wattbit does not implement automatic calibration"),
        writes_verified: true,
        keys: Vec::new(),
    }
}

/// The status from the specification: held at the 80% limit on AC.
fn suppressed_report() -> StatusReport {
    StatusReport {
        version: "0.1.0".into(),
        battery: Some(BatteryTelemetry {
            charge_percent: 80,
            power_source: PowerSource::Ac,
            is_charging: false,
            fully_charged: false,
            external_connected: Some(true),
            full_capacity_mah: Some(8648),
            design_capacity_mah: Some(8694),
            remaining_capacity_mah: Some(6918),
            voltage_mv: Some(11_890),
            amperage_ma: Some(0),
            cycle_count: Some(112),
            health: Some("Good".into()),
            time_to_empty_min: None,
            time_to_full_min: None,
        }),
        battery_error: None,
        charging: ChargingStatus {
            charging_allowed: Some(false),
            policy_state: Some(ChargeState::ChargingSuppressed),
            adapter_enabled: Some(true),
            last_error: None,
        },
        config: Config {
            charge_limit: Some(Thresholds::new(80, None).unwrap()),
            ..Config::default()
        },
        config_warning: None,
        mode: ChargeControlMode::Native,
        capabilities: capabilities(),
        conflicts: Vec::new(),
    }
}

#[test]
fn conflicts_explain_the_pause() {
    let mut report = suppressed_report();
    let conflict = |pid| Conflict {
        tool: "batt".into(),
        pid: Some(pid),
        detail: "/usr/local/bin/batt daemon".into(),
        remedy: "sudo launchctl bootout system/cc.chlc.batt".into(),
    };
    report.conflicts = vec![conflict(7125), conflict(7126)];
    let text = render_status(&report);
    assert!(text.contains(
        "    Paused: batt (pids 7125, 7126) also controls charging. Run `wattbit doctor` to remove it.\n"
    ));
}

#[test]
fn unsupported_mode_points_to_doctor() {
    let mut report = suppressed_report();
    report.mode = ChargeControlMode::Unsupported;
    report.charging.charging_allowed = None;
    let text = render_status(&report);
    assert!(text.contains("    This Mac has no charge-inhibit SMC key; run `wattbit doctor`"));
}

#[test]
fn matches_specified_layout_exactly() {
    let expected = "\
Charging status:
  Allow charging: ✘ (refreshes can take up to 2 minutes)
    Your Mac will not charge, because your current charge is above the limit.
  Use power adapter: ✔

Battery status:
  Current charge: 80%
  State: not charging
  Full capacity: 8648 mAh / 8694 mAh (99%)
  Charge rate: +0.0 W
  Voltage: 11.89 V

Battery configuration:
  Upper limit: 80%
  Lower limit: 75%
  Prevent idle-sleep when charging: ✔
  Disable charging before sleep if charge limit is enabled: ✔
  Prevent system-sleep when charging: ✘
  Prevent sleep while adapter is disabled: ✘
  Allow non-root users to access the daemon: ✔
  Control MagSafe LED: ✘

Hardware compatibility:
  Charge control mode: native
  Sleep hooks: ✔
  MagSafe LED control: ✘
  Power adapter control: ✔
  Auto calibration: ✘

Calibration status:
  Phase: Idle
  Schedule: disabled
";
    assert_eq!(render_status(&suppressed_report()), expected);
}

#[test]
fn explanation_only_when_policy_suppresses() {
    let explanation = "Your Mac will not charge";

    let mut report = suppressed_report();
    report.charging.charging_allowed = Some(true);
    report.charging.policy_state = Some(ChargeState::ChargingPermitted);
    let text = render_status(&report);
    assert!(text.contains("Allow charging: ✔"));
    assert!(!text.contains(explanation));

    report.config.charge_limit = None;
    report.charging.policy_state = None;
    let text = render_status(&report);
    assert!(!text.contains(explanation));
    assert!(text.contains("Upper limit: disabled"));

    // Waiting inside the deadband gets its own, accurate explanation.
    let mut report = suppressed_report();
    report.charging.policy_state = Some(ChargeState::WaitingToResumeCharging);
    let text = render_status(&report);
    assert!(!text.contains(explanation));
    assert!(text.contains("resume charging once the charge drops below 75%"));
}

#[test]
fn unknown_states_render_as_unsupported() {
    let mut report = suppressed_report();
    report.charging.charging_allowed = None;
    report.charging.adapter_enabled = None;
    report.mode = ChargeControlMode::Unsupported;
    report.capabilities.adapter_control = Support::unsupported("absent");
    let text = render_status(&report);
    assert!(text.contains("  Allow charging: unsupported (refreshes"));
    assert!(text.contains("  Use power adapter: unsupported\n"));
    assert!(text.contains("  Charge control mode: unsupported\n"));
    assert!(text.contains("  Power adapter control: ✘\n"));
    assert!(!text.contains("Your Mac will not charge"));
}

#[test]
fn unverified_and_fallback_are_distinguished() {
    let mut report = suppressed_report();
    report.mode = ChargeControlMode::AdapterFallback;
    report.capabilities.adapter_control = Support::unverified("not root");
    let text = render_status(&report);
    assert!(text.contains("  Charge control mode: adapter fallback\n"));
    assert!(text.contains("  Power adapter control: ✔ (unverified)\n"));
}

#[test]
fn fallback_explains_running_from_battery() {
    let mut report = suppressed_report();
    report.mode = ChargeControlMode::AdapterFallback;
    report.charging.adapter_enabled = Some(false);
    for state in [
        ChargeState::ChargingSuppressed,
        ChargeState::WaitingToResumeCharging,
    ] {
        report.charging.policy_state = Some(state);
        let text = render_status(&report);
        assert!(text.contains(
            "    The charger is switched off, so your Mac runs from its battery until it drops \
             below 75%.\n"
        ));
        assert!(text.contains("System Settings → Battery → Charging"));
        assert!(!text.contains("Your Mac will not charge"));
    }

    report.charging.policy_state = Some(ChargeState::ChargingPermitted);
    report.charging.charging_allowed = Some(true);
    report.charging.adapter_enabled = Some(true);
    assert!(!render_status(&report).contains("The charger is switched off"));
}

#[test]
fn discharge_rate_and_missing_battery() {
    let mut report = suppressed_report();
    if let Some(battery) = report.battery.as_mut() {
        battery.amperage_ma = Some(-1500);
        battery.power_source = PowerSource::Battery;
    }
    let text = render_status(&report);
    assert!(text.contains("  Charge rate: -17.8 W\n"));
    assert!(text.contains("  State: discharging\n"));

    report.battery = None;
    report.battery_error = Some("this Mac has no internal battery".into());
    report.config_warning = Some("bad config; defaults are in use".into());
    let text = render_status(&report);
    assert!(text.contains("Battery status:\n  unavailable: this Mac has no internal battery\n"));
    assert!(text.ends_with("\nWarning: bad config; defaults are in use\n"));
}
