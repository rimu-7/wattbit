//! Read-only smoke tests against the real hardware of the machine running
//! them. They never write to the SMC. Run with:
//!
//! ```sh
//! cargo test --test hardware -- --ignored --nocapture
//! ```

use berry::battery;
use berry::capabilities;
use berry::conflicts;

#[test]
#[ignore = "inspects this machine's processes"]
fn report_other_charge_limiters() {
    let running = conflicts::running();
    for c in &running {
        println!("running   {} pid {:?}: {}", c.tool, c.pid, c.detail);
    }
    for c in conflicts::installed(&running) {
        println!("installed {}: {}", c.tool, c.detail);
    }
}
use berry::smc::{keys, Smc, SmcError};

#[test]
#[ignore = "touches real hardware"]
fn capability_detection_without_write_tests() {
    let smc = Smc::open();
    let detection = capabilities::detect(smc.as_ref(), false);
    let caps = &detection.capabilities;
    println!("{}", serde_json::to_string_pretty(caps).unwrap());
    assert!(!caps.writes_verified);
    // Without root nothing may be reported as verified-supported.
    for support in [
        &caps.charge_inhibit,
        &caps.adapter_control,
        &caps.firmware_limit,
    ] {
        assert_ne!(*support, capabilities::Support::Supported);
    }
}

#[test]
#[ignore = "touches real hardware"]
fn battery_telemetry_is_plausible() {
    match battery::read() {
        Ok(t) => {
            println!("{t:#?}");
            assert!(t.charge_percent <= 100);
            if let (Some(full), Some(design)) = (t.full_capacity_mah, t.design_capacity_mah) {
                assert!(full > 0 && design > 0);
            }
        }
        Err(battery::BatteryError::NoBattery) => println!("no internal battery (desktop Mac)"),
        Err(e) => panic!("battery read failed: {e}"),
    }
}

#[test]
#[ignore = "touches real hardware"]
fn smc_known_keys_are_readable_or_absent() {
    let smc = Smc::open().expect("AppleSMC should be reachable on every Mac");
    for descriptor in keys::KNOWN_KEYS {
        match smc.read(descriptor.key) {
            Ok(value) => println!(
                "{} type={:?} size={} value=[{}]  ({})",
                descriptor.key,
                value.info.data_type.to_string(),
                value.info.size,
                value.hex(),
                descriptor.purpose
            ),
            Err(SmcError::KeyNotFound(_)) => {
                println!("{} absent  ({})", descriptor.key, descriptor.purpose)
            }
            Err(other) => panic!("unexpected error reading {}: {other}", descriptor.key),
        }
    }
}
