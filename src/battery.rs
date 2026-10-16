//! Battery telemetry.
//!
//! Combines two native sources:
//! - `IOPSCopyPowerSourcesInfo` / `IOPSGetPowerSourceDescription` for the
//!   user-visible charge percentage, power source state and health.
//! - The `AppleSmartBattery` registry entry (an `IOPMPowerSource`) for
//!   electrical detail: capacities in mAh, voltage, amperage, cycle count.
//!
//! Registry layouts differ between firmware generations: older releases
//! publish `AppleRawMaxCapacity` and friends at the top level, while newer
//! ones nest raw capacities inside a `BatteryData` dictionary. Both are read.

use std::ffi::CStr;
use std::ptr;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::kCFAllocatorDefault;
use core_foundation_sys::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};
use serde::{Deserialize, Serialize};

use crate::ffi;

type Dict = CFDictionary<CFString, CFType>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

/// What the battery is doing right now, as reported by the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeActivity {
    Charging,
    NotCharging,
    FullyCharged,
    Discharging,
}

impl ChargeActivity {
    pub fn label(self) -> &'static str {
        match self {
            ChargeActivity::Charging => "charging",
            ChargeActivity::NotCharging => "not charging",
            ChargeActivity::FullyCharged => "fully charged",
            ChargeActivity::Discharging => "discharging",
        }
    }
}

/// A snapshot of battery state. Fields the hardware does not publish are
/// `None` rather than guessed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatteryTelemetry {
    pub charge_percent: u8,
    pub power_source: PowerSource,
    pub is_charging: bool,
    pub fully_charged: bool,
    pub external_connected: Option<bool>,
    pub full_capacity_mah: Option<u32>,
    pub design_capacity_mah: Option<u32>,
    pub remaining_capacity_mah: Option<u32>,
    pub voltage_mv: Option<u32>,
    /// Signed battery current: positive while charging.
    pub amperage_ma: Option<i32>,
    pub cycle_count: Option<u32>,
    pub health: Option<String>,
    pub time_to_empty_min: Option<u32>,
    pub time_to_full_min: Option<u32>,
}

impl BatteryTelemetry {
    pub fn activity(&self) -> ChargeActivity {
        if self.is_charging {
            ChargeActivity::Charging
        } else if self.power_source == PowerSource::Ac {
            if self.fully_charged {
                ChargeActivity::FullyCharged
            } else {
                ChargeActivity::NotCharging
            }
        } else {
            ChargeActivity::Discharging
        }
    }

    pub fn on_ac(&self) -> bool {
        self.power_source == PowerSource::Ac
    }

    /// Battery power in watts: positive while charging, negative while
    /// discharging.
    pub fn power_watts(&self) -> Option<f64> {
        let volts = f64::from(self.voltage_mv?) / 1000.0;
        let amps = f64::from(self.amperage_ma?) / 1000.0;
        Some(volts * amps)
    }

    /// Full-charge capacity as a percentage of design capacity.
    pub fn health_percent(&self) -> Option<u32> {
        let full = f64::from(self.full_capacity_mah?);
        let design = f64::from(self.design_capacity_mah?);
        (design > 0.0).then(|| (full * 100.0 / design).round() as u32)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BatteryError {
    #[error("IOPSCopyPowerSourcesInfo returned no data")]
    NoPowerSourceInfo,
    #[error("this Mac has no internal battery")]
    NoBattery,
}

/// Reads current battery telemetry.
pub fn read() -> Result<BatteryTelemetry, BatteryError> {
    let source = internal_battery_description()?;
    let registry = smart_battery_properties();
    Ok(merge(&source, registry.as_ref()))
}

fn internal_battery_description() -> Result<Dict, BatteryError> {
    // SAFETY: Copy-rule function; ownership is transferred to `blob` below.
    let blob = unsafe { ffi::IOPSCopyPowerSourcesInfo() };
    if blob.is_null() {
        return Err(BatteryError::NoPowerSourceInfo);
    }
    // SAFETY: `blob` is a non-null, owned CF object.
    let blob = unsafe { CFType::wrap_under_create_rule(blob) };
    // SAFETY: `blob` is a valid power-sources blob; the result is owned.
    let list = unsafe { ffi::IOPSCopyPowerSourcesList(blob.as_CFTypeRef()) };
    if list.is_null() {
        return Err(BatteryError::NoBattery);
    }
    // SAFETY: `list` is a non-null, owned CFArray of CF objects.
    let list: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(list) };
    for item in list.iter() {
        // SAFETY: `item` comes from the list built from `blob`, which is
        // still alive. The result follows the Get rule.
        let description =
            unsafe { ffi::IOPSGetPowerSourceDescription(blob.as_CFTypeRef(), item.as_CFTypeRef()) };
        if description.is_null() {
            continue;
        }
        // SAFETY: non-null Get-rule dictionary; wrapping retains it so it
        // outlives `blob`.
        let description: Dict = unsafe { Dict::wrap_under_get_rule(description) };
        let is_internal = string(&description, ffi::kIOPSTypeKey).as_deref()
            == Some(ffi::kIOPSInternalBatteryType);
        let present = boolean(&description, ffi::kIOPSIsPresentKey).unwrap_or(true);
        if is_internal && present {
            return Ok(description);
        }
    }
    Err(BatteryError::NoBattery)
}

fn smart_battery_properties() -> Option<Dict> {
    registry_properties(c"AppleSmartBattery")
}

/// Copies the property table of the first registry entry matching `class`.
fn registry_properties(class: &CStr) -> Option<Dict> {
    // SAFETY: `class` is NUL-terminated; the dictionary is consumed below.
    let matching = unsafe { ffi::IOServiceMatching(class.as_ptr()) };
    if matching.is_null() {
        return None;
    }
    // SAFETY: consumes `matching`.
    let service = unsafe {
        ffi::IOServiceGetMatchingService(ffi::IO_MAIN_PORT_DEFAULT, matching.cast_const())
    };
    if service == ffi::IO_OBJECT_NULL {
        return None;
    }
    let mut properties: CFMutableDictionaryRef = ptr::null_mut();
    // SAFETY: `service` is live and `properties` is a valid out-pointer.
    // The service handle is released immediately after.
    let kr = unsafe {
        let kr = ffi::IORegistryEntryCreateCFProperties(
            service,
            &mut properties,
            kCFAllocatorDefault,
            0,
        );
        ffi::IOObjectRelease(service);
        kr
    };
    if kr != ffi::kIOReturnSuccess || properties.is_null() {
        return None;
    }
    // SAFETY: non-null, owned (Create-rule) dictionary.
    Some(unsafe { Dict::wrap_under_create_rule(properties as CFDictionaryRef) })
}

fn merge(source: &Dict, registry: Option<&Dict>) -> BatteryTelemetry {
    let reg = |key: &str| registry.and_then(|r| number(r, key));
    let reg_bool = |key: &str| registry.and_then(|r| boolean(r, key));
    let battery_data = registry.and_then(|r| dictionary(r, "BatteryData"));
    let nested = |key: &str| battery_data.as_ref().and_then(|d| number(d, key));

    let charge_percent = charge_percent(
        number(source, ffi::kIOPSCurrentCapacityKey),
        number(source, ffi::kIOPSMaxCapacityKey),
    )
    .or_else(|| charge_percent(reg("CurrentCapacity"), reg("MaxCapacity")))
    .unwrap_or(0);

    let power_source = match string(source, ffi::kIOPSPowerSourceStateKey).as_deref() {
        Some(ffi::kIOPSACPowerValue) => PowerSource::Ac,
        Some(ffi::kIOPSBatteryPowerValue) => PowerSource::Battery,
        _ => PowerSource::Unknown,
    };

    // Intel firmware reports MaxCapacity/CurrentCapacity in mAh; Apple
    // Silicon reports them as percentages (MaxCapacity == 100).
    let legacy_mah = reg("MaxCapacity").filter(|max| *max > 100).is_some();

    BatteryTelemetry {
        charge_percent,
        power_source,
        is_charging: boolean(source, ffi::kIOPSIsChargingKey)
            .or_else(|| reg_bool("IsCharging"))
            .unwrap_or(false),
        fully_charged: boolean(source, ffi::kIOPSIsChargedKey)
            .or_else(|| reg_bool("FullyCharged"))
            .unwrap_or(false),
        external_connected: reg_bool("ExternalConnected"),
        full_capacity_mah: reg("AppleRawMaxCapacity")
            .or_else(|| nested("FullChargeCapacity"))
            .or_else(|| legacy_mah.then(|| reg("MaxCapacity")).flatten())
            .and_then(positive_u32),
        design_capacity_mah: reg("DesignCapacity")
            .or_else(|| nested("DesignCapacity"))
            .and_then(positive_u32),
        remaining_capacity_mah: reg("AppleRawCurrentCapacity")
            .or_else(|| nested("RemainingCapacity"))
            .or_else(|| legacy_mah.then(|| reg("CurrentCapacity")).flatten())
            .and_then(non_negative_u32),
        voltage_mv: reg("Voltage")
            .or_else(|| number(source, ffi::kIOPSVoltageKey))
            .and_then(positive_u32),
        amperage_ma: reg("Amperage")
            .or_else(|| number(source, ffi::kIOPSCurrentKey))
            .map(decode_amperage),
        cycle_count: reg("CycleCount").and_then(non_negative_u32),
        health: string(source, ffi::kIOPSBatteryHealthKey),
        time_to_empty_min: number(source, ffi::kIOPSTimeToEmptyKey).and_then(minutes),
        time_to_full_min: number(source, ffi::kIOPSTimeToFullChargeKey).and_then(minutes),
    }
}

/// Converts a current/max capacity pair into a 0–100 percentage.
pub fn charge_percent(current: Option<i64>, max: Option<i64>) -> Option<u8> {
    let (current, max) = (current?, max?);
    if max <= 0 || current < 0 {
        return None;
    }
    let percent = (current as f64 * 100.0 / max as f64).round();
    Some(percent.clamp(0.0, 100.0) as u8)
}

/// The registry publishes amperage as a 64-bit two's-complement value
/// (e.g. 18446744073709551304 for -312 mA). CFNumber hands it back as a
/// signed 64-bit integer; values outside the i32 range are truncated to
/// their low 32 bits, which preserves the sign for 32-bit encodings.
pub fn decode_amperage(raw: i64) -> i32 {
    i32::try_from(raw).unwrap_or(raw as i32)
}

fn positive_u32(value: i64) -> Option<u32> {
    u32::try_from(value).ok().filter(|v| *v > 0)
}

fn non_negative_u32(value: i64) -> Option<u32> {
    u32::try_from(value).ok()
}

/// IOPS uses -1 for "still calculating" and 65535 for "not applicable".
fn minutes(value: i64) -> Option<u32> {
    (0..65_535).contains(&value).then_some(value as u32)
}

fn number(dict: &Dict, key: &str) -> Option<i64> {
    dict.find(CFString::new(key))?
        .downcast::<CFNumber>()?
        .to_i64()
}

fn boolean(dict: &Dict, key: &str) -> Option<bool> {
    dict.find(CFString::new(key))?
        .downcast::<CFBoolean>()
        .map(bool::from)
}

fn string(dict: &Dict, key: &str) -> Option<String> {
    dict.find(CFString::new(key))?
        .downcast::<CFString>()
        .map(|s| s.to_string())
}

fn dictionary(dict: &Dict, key: &str) -> Option<Dict> {
    let value = dict.find(CFString::new(key))?;
    let untyped = value.downcast::<CFDictionary>()?;
    // SAFETY: the reference is a live CFDictionary; wrapping under the Get
    // rule retains it. Key and value types are checked at each lookup.
    Some(unsafe { Dict::wrap_under_get_rule(untyped.as_concrete_TypeRef()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BatteryTelemetry {
        BatteryTelemetry {
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
            cycle_count: Some(42),
            health: Some("Good".into()),
            time_to_empty_min: None,
            time_to_full_min: None,
        }
    }

    #[test]
    fn amperage_handles_twos_complement() {
        assert_eq!(decode_amperage(-312), -312);
        assert_eq!(decode_amperage(1500), 1500);
        assert_eq!(decode_amperage(0xFFFF_FEC8), -312);
    }

    #[test]
    fn percent_is_rounded_and_clamped() {
        assert_eq!(charge_percent(Some(80), Some(100)), Some(80));
        assert_eq!(charge_percent(Some(3291), Some(3934)), Some(84));
        assert_eq!(charge_percent(Some(120), Some(100)), Some(100));
        assert_eq!(charge_percent(Some(10), Some(0)), None);
        assert_eq!(charge_percent(None, Some(100)), None);
    }

    #[test]
    fn derived_values() {
        let mut t = sample();
        assert_eq!(t.health_percent(), Some(99));
        assert_eq!(t.activity(), ChargeActivity::NotCharging);
        t.amperage_ma = Some(-2000);
        assert!((t.power_watts().unwrap() + 23.78).abs() < 1e-9);
        t.power_source = PowerSource::Battery;
        assert_eq!(t.activity(), ChargeActivity::Discharging);
        t.is_charging = true;
        assert_eq!(t.activity(), ChargeActivity::Charging);
    }

    #[test]
    fn minutes_filters_sentinels() {
        assert_eq!(minutes(-1), None);
        assert_eq!(minutes(65_535), None);
        assert_eq!(minutes(90), Some(90));
    }
}
