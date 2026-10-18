//! Hardware control used by the daemon.
//!
//! Applies charge-inhibit, adapter, firmware-limit and LED states through
//! the controls chosen during capability detection. Every SMC write is
//! verified by reading it back.

use crate::battery::{self, BatteryError, BatteryTelemetry};
use crate::capabilities::{self, Capabilities, Detection};
use crate::mcl::{Mcl, MclError};
use crate::smc::keys::{self, BinaryControl, FourCC, LedMode};
use crate::smc::{hex, Smc, SmcError};

#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Smc(#[from] SmcError),
    #[error(transparent)]
    Mcl(#[from] MclError),
    #[error("SMC key {key} holds unrecognized value [{value}]")]
    UnknownState { key: FourCC, value: String },
}

#[derive(Debug)]
pub struct Hardware {
    smc: Option<Smc>,
    detection: Detection,
}

impl Hardware {
    /// Opens the SMC and probes capabilities. Pass `verify_writes = true`
    /// only when running as root.
    pub fn open(verify_writes: bool) -> Self {
        let smc = Smc::open();
        let detection = capabilities::detect(smc.as_ref(), verify_writes);
        Self {
            smc: smc.ok(),
            detection,
        }
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.detection.capabilities
    }

    pub fn capabilities_mut(&mut self) -> &mut Capabilities {
        &mut self.detection.capabilities
    }

    pub fn battery(&self) -> Result<BatteryTelemetry, BatteryError> {
        battery::read()
    }

    fn smc(&self) -> Result<&Smc, HardwareError> {
        self.smc
            .as_ref()
            .ok_or_else(|| HardwareError::Unsupported("the SMC is not available".into()))
    }

    fn control_engaged(&self, control: &BinaryControl) -> Result<bool, HardwareError> {
        let smc = self.smc()?;
        let mut engaged = false;
        for key in control.keys {
            let value = smc.read(*key)?;
            match control.classify(&value.bytes) {
                Some(state) => engaged |= state,
                None => {
                    return Err(HardwareError::UnknownState {
                        key: *key,
                        value: hex(&value.bytes),
                    })
                }
            }
        }
        Ok(engaged)
    }

    fn set_control(&self, control: &BinaryControl, engaged: bool) -> Result<(), HardwareError> {
        let smc = self.smc()?;
        for key in control.keys {
            smc.write_verified(*key, control.pattern(engaged))?;
        }
        Ok(())
    }

    fn charge_inhibit(&self) -> Result<&'static BinaryControl, HardwareError> {
        self.detection.charge_inhibit.ok_or_else(|| {
            HardwareError::Unsupported(format!(
                "charge inhibit is not supported on this Mac: {}",
                self.capabilities()
                    .charge_inhibit
                    .reason()
                    .unwrap_or("unknown reason")
            ))
        })
    }

    fn adapter(&self) -> Result<&'static BinaryControl, HardwareError> {
        self.detection.adapter.ok_or_else(|| {
            HardwareError::Unsupported(format!(
                "adapter control is not supported on this Mac: {}",
                self.capabilities()
                    .adapter_control
                    .reason()
                    .unwrap_or("unknown reason")
            ))
        })
    }

    pub fn charging_inhibited(&self) -> Result<bool, HardwareError> {
        self.control_engaged(self.charge_inhibit()?)
    }

    pub fn set_charging_inhibited(&self, inhibited: bool) -> Result<(), HardwareError> {
        self.set_control(self.charge_inhibit()?, inhibited)
    }

    pub fn adapter_disabled(&self) -> Result<bool, HardwareError> {
        self.control_engaged(self.adapter()?)
    }

    pub fn set_adapter_disabled(&self, disabled: bool) -> Result<(), HardwareError> {
        self.set_control(self.adapter()?, disabled)
    }

    fn require_firmware_limit(&self) -> Result<(), HardwareError> {
        let support = &self.capabilities().firmware_limit;
        if support.is_usable() {
            Ok(())
        } else {
            Err(HardwareError::Unsupported(format!(
                "BCLM is not supported on this Mac: {}",
                support.reason().unwrap_or("unknown reason")
            )))
        }
    }

    pub fn firmware_limit(&self) -> Result<u8, HardwareError> {
        self.require_firmware_limit()?;
        let value = self.smc()?.read(keys::BCLM)?;
        match value.bytes.as_slice() {
            [percent] if *percent <= keys::BCLM_MAX => Ok(*percent),
            other => Err(HardwareError::UnknownState {
                key: keys::BCLM,
                value: hex(other),
            }),
        }
    }

    pub fn set_firmware_limit(&self, percent: u8) -> Result<(), HardwareError> {
        self.require_firmware_limit()?;
        if percent > keys::BCLM_MAX {
            return Err(HardwareError::Unsupported(format!(
                "BCLM accepts at most {}%, got {percent}%",
                keys::BCLM_MAX
            )));
        }
        Ok(self.smc()?.write_verified(keys::BCLM, &[percent])?)
    }

    pub fn set_led(&self, mode: LedMode) -> Result<(), HardwareError> {
        let support = &self.capabilities().magsafe_led;
        if !support.is_usable() {
            return Err(HardwareError::Unsupported(format!(
                "MagSafe LED control is not supported: {}",
                support.reason().unwrap_or("unknown reason")
            )));
        }
        Ok(self.smc()?.write_verified(keys::ACLC, &[mode.byte()])?)
    }

    pub fn mcl(&self) -> Result<&Mcl, HardwareError> {
        self.detection.mcl.as_ref().ok_or_else(|| {
            HardwareError::Unsupported(format!(
                "native charge limit (MCL) is not supported on this Mac: {}",
                self.capabilities().mcl.reason().unwrap_or("unknown reason")
            ))
        })
    }

    pub fn mcl_limit(&self) -> Result<u8, HardwareError> {
        Ok(self.mcl()?.get_limit()?)
    }

    pub fn is_mcl_enabled(&self) -> Result<bool, HardwareError> {
        Ok(self.mcl()?.is_enabled()?)
    }

    pub fn set_mcl_limit(&self, percent: u8) -> Result<(), HardwareError> {
        Ok(self.mcl()?.set_limit(percent)?)
    }

    pub fn disable_mcl(&self) -> Result<(), HardwareError> {
        Ok(self.mcl()?.disable()?)
    }
}
