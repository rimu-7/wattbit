//! Hardware feature discovery.
//!
//! A control is reported as supported only when every SMC key it needs is
//! present with the expected size and currently holds one of the control's
//! two known byte patterns. When running as root, write access is then
//! verified by writing back the value just read and reading it again, which
//! leaves hardware state unchanged. Without root, usable controls are
//! reported as `unverified`.

use std::collections::HashMap;
use std::ffi::CStr;
use std::ptr;

use serde::{Deserialize, Serialize};

use crate::smc::keys::{self, BinaryControl, FourCC, LedMode};
use crate::smc::{Smc, SmcError, SmcValue};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Support {
    Supported,
    /// Present and plausible, but write access was not tested.
    Unverified {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
}

impl Support {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Support::Unsupported {
            reason: reason.into(),
        }
    }

    pub fn unverified(reason: impl Into<String>) -> Self {
        Support::Unverified {
            reason: reason.into(),
        }
    }

    /// Whether wattbit will attempt to use the feature.
    pub fn is_usable(&self) -> bool {
        !matches!(self, Support::Unsupported { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Support::Supported => None,
            Support::Unverified { reason } | Support::Unsupported { reason } => Some(reason),
        }
    }
}

/// How the charge limit is enforced on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChargeControlMode {
    /// SMC charge-inhibit keys, driven by wattbit's hysteresis loop.
    Native,
    /// macOS native charge limit (MCL via PowerUI); firmware holds the limit and powers from the charger.
    Mcl,
    /// Intel `BCLM` firmware ceiling; the firmware enforces the limit.
    FirmwareLimit,
    /// Adapter bypass used to hold the limit (opt-in, runs on battery).
    AdapterFallback,
    Unsupported,
}

impl ChargeControlMode {
    pub fn label(self) -> &'static str {
        match self {
            ChargeControlMode::Native => "native",
            ChargeControlMode::Mcl => "native limit (macOS MCL)",
            ChargeControlMode::FirmwareLimit => "firmware limit (BCLM)",
            ChargeControlMode::AdapterFallback => "adapter fallback",
            ChargeControlMode::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Architecture of the hardware, not of this binary.
    pub arch: String,
    pub apple_silicon: bool,
    /// This process runs under Rosetta 2.
    pub translated: bool,
    pub model: Option<String>,
    pub os_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyProbe {
    pub key: String,
    pub purpose: String,
    pub present: bool,
    pub readable: bool,
    /// `None` when write access was not tested.
    pub writable: Option<bool>,
    pub data_type: Option<String>,
    pub size: Option<u32>,
    /// Raw bytes as hex, in SMC order.
    pub value: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub host: HostInfo,
    pub smc: Support,
    pub charge_inhibit: Support,
    /// Name of the selected charge-inhibit control, e.g. `CH0B/CH0C`.
    pub charge_inhibit_control: Option<String>,
    pub mcl: Support,
    pub mcl_available_limits: Vec<u8>,
    pub firmware_limit: Support,
    pub adapter_control: Support,
    pub adapter_control_keys: Option<String>,
    pub magsafe_led: Support,
    pub sleep_hooks: Support,
    pub calibration: Support,
    pub writes_verified: bool,
    pub keys: Vec<KeyProbe>,
}

impl Capabilities {
    /// The enforcement mode in effect for the given configuration.
    pub fn charge_control_mode(&self, adapter_fallback: bool) -> ChargeControlMode {
        if self.charge_inhibit.is_usable() {
            ChargeControlMode::Native
        } else if self.mcl.is_usable() {
            ChargeControlMode::Mcl
        } else if self.firmware_limit.is_usable() {
            ChargeControlMode::FirmwareLimit
        } else if adapter_fallback && self.adapter_control.is_usable() {
            ChargeControlMode::AdapterFallback
        } else {
            ChargeControlMode::Unsupported
        }
    }
}

/// Capabilities plus the concrete controls selected for the hardware layer.
#[derive(Debug, Clone)]
pub struct Detection {
    pub capabilities: Capabilities,
    pub charge_inhibit: Option<&'static BinaryControl>,
    pub adapter: Option<&'static BinaryControl>,
    pub mcl: Option<crate::mcl::Mcl>,
}

/// Probes the SMC. Pass `verify_writes = true` only when running as root.
pub fn detect(smc: Result<&Smc, &SmcError>, verify_writes: bool) -> Detection {
    let host = host_info();
    let calibration = Support::unsupported("wattbit does not implement automatic calibration");
    let sleep_hooks = Support::unverified("sleep hooks are registered by the daemon");

    let (mcl_obj, mcl_support, mcl_limits) = match crate::mcl::Mcl::open() {
        Ok(m) => {
            let limits = m.available_limits().to_vec();
            (Some(m), Support::Supported, limits)
        }
        Err(e) => (None, Support::unsupported(e.to_string()), Vec::new()),
    };

    let smc = match smc {
        Ok(smc) => smc,
        Err(e) => {
            let reason = format!("SMC unavailable: {e}");
            return Detection {
                capabilities: Capabilities {
                    host,
                    smc: Support::unsupported(reason.clone()),
                    charge_inhibit: Support::unsupported(reason.clone()),
                    charge_inhibit_control: None,
                    mcl: mcl_support,
                    mcl_available_limits: mcl_limits,
                    firmware_limit: Support::unsupported(reason.clone()),
                    adapter_control: Support::unsupported(reason.clone()),
                    adapter_control_keys: None,
                    magsafe_led: Support::unsupported(reason),
                    sleep_hooks,
                    calibration,
                    writes_verified: false,
                    keys: Vec::new(),
                },
                charge_inhibit: None,
                adapter: None,
                mcl: mcl_obj,
            };
        }
    };

    let mut prober = Prober::new(smc, verify_writes);
    let (charge_inhibit, charge_support) = prober.select(keys::CHARGE_INHIBIT_CONTROLS);
    let (adapter, adapter_support) = prober.select(keys::ADAPTER_CONTROLS);
    let firmware_limit = prober.firmware_limit();
    let magsafe_led = prober.magsafe_led();

    Detection {
        capabilities: Capabilities {
            host,
            smc: Support::Supported,
            charge_inhibit: charge_support,
            charge_inhibit_control: charge_inhibit.map(|c| c.name.to_owned()),
            mcl: mcl_support,
            mcl_available_limits: mcl_limits,
            firmware_limit,
            adapter_control: adapter_support,
            adapter_control_keys: adapter.map(|c| c.name.to_owned()),
            magsafe_led,
            sleep_hooks,
            calibration,
            writes_verified: verify_writes,
            keys: prober.into_probes(),
        },
        charge_inhibit,
        adapter,
        mcl: mcl_obj,
    }
}

struct Prober<'a> {
    smc: &'a Smc,
    verify_writes: bool,
    values: HashMap<FourCC, SmcValue>,
    probes: Vec<(FourCC, KeyProbe)>,
}

impl<'a> Prober<'a> {
    fn new(smc: &'a Smc, verify_writes: bool) -> Self {
        let mut values = HashMap::new();
        let mut probes = Vec::new();
        for descriptor in keys::KNOWN_KEYS {
            let mut probe = KeyProbe {
                key: descriptor.key.to_string(),
                purpose: descriptor.purpose.to_owned(),
                present: false,
                readable: false,
                writable: None,
                data_type: None,
                size: None,
                value: None,
                note: None,
            };
            match smc.read(descriptor.key) {
                Ok(value) => {
                    probe.present = true;
                    probe.readable = true;
                    probe.data_type = Some(value.info.data_type.to_string());
                    probe.size = Some(value.info.size);
                    probe.value = Some(value.hex());
                    values.insert(descriptor.key, value);
                }
                Err(SmcError::KeyNotFound(_)) => {}
                Err(e) => {
                    // The key exists but could not be read.
                    probe.present = !matches!(e, SmcError::Call { .. });
                    probe.note = Some(e.to_string());
                }
            }
            probes.push((descriptor.key, probe));
        }
        Self {
            smc,
            verify_writes,
            values,
            probes,
        }
    }

    fn probe_mut(&mut self, key: FourCC) -> Option<&mut KeyProbe> {
        self.probes
            .iter_mut()
            .find(|(k, _)| *k == key)
            .map(|(_, probe)| probe)
    }

    /// Returns the first usable control from `candidates`, in preference order.
    fn select(
        &mut self,
        candidates: &[&'static BinaryControl],
    ) -> (Option<&'static BinaryControl>, Support) {
        let mut reasons = Vec::new();
        for &control in candidates {
            let support = self.check_control(control);
            if support.is_usable() {
                return (Some(control), support);
            }
            if let Some(reason) = support.reason() {
                reasons.push(format!("{}: {reason}", control.name));
            }
        }
        (None, Support::unsupported(reasons.join("; ")))
    }

    fn check_control(&mut self, control: &BinaryControl) -> Support {
        for key in control.keys {
            let Some(value) = self.values.get(key) else {
                return Support::unsupported(format!("SMC key {key} is not present"));
            };
            if value.info.size != control.size {
                return Support::unsupported(format!(
                    "SMC key {key} has size {}, expected {}",
                    value.info.size, control.size
                ));
            }
            if control.classify(&value.bytes).is_none() {
                return Support::unsupported(format!(
                    "SMC key {key} holds unrecognized value [{}]; wattbit will not drive it",
                    value.hex()
                ));
            }
        }
        if !self.verify_writes {
            return Support::unverified("write access is only tested when running as root");
        }
        for key in control.keys {
            let current = self.values[key].bytes.clone();
            let result = self.smc.write_verified(*key, &current);
            let probe = self.probe_mut(*key);
            match result {
                Ok(()) => {
                    if let Some(probe) = probe {
                        probe.writable = Some(true);
                    }
                }
                Err(e) => {
                    if let Some(probe) = probe {
                        probe.writable = Some(false);
                        probe.note = Some(e.to_string());
                    }
                    return Support::unsupported(format!("write test on {key} failed: {e}"));
                }
            }
        }
        Support::Supported
    }

    fn firmware_limit(&mut self) -> Support {
        let key = keys::BCLM;
        let Some(value) = self.values.get(&key) else {
            return Support::unsupported("SMC key BCLM is not present on this firmware");
        };
        if value.bytes.len() != 1 || value.bytes[0] > keys::BCLM_MAX {
            return Support::unsupported(format!(
                "SMC key BCLM holds unexpected value [{}]",
                value.hex()
            ));
        }
        self.write_back(key, "BCLM")
    }

    fn magsafe_led(&mut self) -> Support {
        let key = keys::ACLC;
        let Some(value) = self.values.get(&key) else {
            return Support::unsupported("SMC key ACLC is not present (no MagSafe LED control)");
        };
        if value.bytes.len() != 1 {
            return Support::unsupported(format!(
                "SMC key ACLC has size {}, expected 1",
                value.bytes.len()
            ));
        }
        // Writing back a non-system value would pin the LED, so the write
        // test only runs while the system owns the LED.
        if value.bytes[0] != LedMode::System.byte() {
            return Support::unverified(format!(
                "ACLC currently holds [{}]; write test skipped to avoid changing the LED",
                value.hex()
            ));
        }
        self.write_back(key, "ACLC")
    }

    fn write_back(&mut self, key: FourCC, name: &str) -> Support {
        if !self.verify_writes {
            return Support::unverified("write access is only tested when running as root");
        }
        let current = self.values[&key].bytes.clone();
        let result = self.smc.write_verified(key, &current);
        let ok = result.is_ok();
        if let Some(probe) = self.probe_mut(key) {
            probe.writable = Some(ok);
            if let Err(e) = &result {
                probe.note = Some(e.to_string());
            }
        }
        match result {
            Ok(()) => Support::Supported,
            Err(e) => Support::unsupported(format!("write test on {name} failed: {e}")),
        }
    }

    fn into_probes(self) -> Vec<KeyProbe> {
        self.probes.into_iter().map(|(_, probe)| probe).collect()
    }
}

/// Describes the machine using `sysctl`.
pub fn host_info() -> HostInfo {
    let translated = sysctl_i32(c"sysctl.proc_translated") == Some(1);
    let apple_silicon = sysctl_i32(c"hw.optional.arm64") == Some(1);
    let arch = if apple_silicon {
        "arm64 (Apple Silicon)"
    } else {
        "x86_64 (Intel)"
    };
    HostInfo {
        arch: arch.to_owned(),
        apple_silicon,
        translated,
        model: sysctl_string(c"hw.model"),
        os_version: sysctl_string(c"kern.osproductversion"),
    }
}

fn sysctl_i32(name: &CStr) -> Option<i32> {
    let mut value: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    // SAFETY: `value` and `len` describe a valid, correctly sized buffer.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut i32).cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<i32>()).then_some(value)
}

fn sysctl_string(name: &CStr) -> Option<String> {
    let mut len = 0usize;
    // SAFETY: a null buffer asks sysctl for the required length.
    let rc =
        unsafe { libc::sysctlbyname(name.as_ptr(), ptr::null_mut(), &mut len, ptr::null_mut(), 0) };
    if rc != 0 || len == 0 || len > 1024 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` is `len` bytes long.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    let text = CStr::from_bytes_until_nul(&buf).ok()?.to_str().ok()?;
    Some(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with_mcl(
        charge: Support,
        mcl: Support,
        bclm: Support,
        adapter: Support,
    ) -> Capabilities {
        Capabilities {
            host: HostInfo {
                arch: "arm64 (Apple Silicon)".into(),
                apple_silicon: true,
                translated: false,
                model: None,
                os_version: None,
            },
            smc: Support::Supported,
            charge_inhibit: charge,
            charge_inhibit_control: None,
            mcl,
            mcl_available_limits: vec![80, 85, 90, 95, 100],
            firmware_limit: bclm,
            adapter_control: adapter,
            adapter_control_keys: None,
            magsafe_led: Support::unsupported("x"),
            sleep_hooks: Support::Supported,
            calibration: Support::unsupported("x"),
            writes_verified: true,
            keys: Vec::new(),
        }
    }

    #[test]
    fn mode_prefers_native_then_mcl_then_firmware_then_fallback() {
        let no = || Support::unsupported("absent");
        let c = caps_with_mcl(
            Support::Supported,
            Support::Supported,
            Support::Supported,
            Support::Supported,
        );
        assert_eq!(c.charge_control_mode(true), ChargeControlMode::Native);

        let c = caps_with_mcl(
            no(),
            Support::Supported,
            Support::Supported,
            Support::Supported,
        );
        assert_eq!(c.charge_control_mode(true), ChargeControlMode::Mcl);

        let c = caps_with_mcl(no(), no(), Support::Supported, Support::Supported);
        assert_eq!(
            c.charge_control_mode(true),
            ChargeControlMode::FirmwareLimit
        );

        let c = caps_with_mcl(no(), no(), no(), Support::Supported);
        assert_eq!(c.charge_control_mode(false), ChargeControlMode::Unsupported);
        assert_eq!(
            c.charge_control_mode(true),
            ChargeControlMode::AdapterFallback
        );

        let c = caps_with_mcl(no(), no(), no(), no());
        assert_eq!(c.charge_control_mode(true), ChargeControlMode::Unsupported);
    }

    #[test]
    fn host_info_reads_sysctl() {
        let host = host_info();
        assert!(host.os_version.is_some());
        #[cfg(target_arch = "aarch64")]
        assert!(host.apple_silicon);
    }
}
