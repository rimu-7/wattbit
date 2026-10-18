//! Persistent configuration, stored as JSON at `/etc/wattbit.json`.
//!
//! Writes are atomic: the new contents go to a temporary file in the same
//! directory, which is fsynced and renamed over the old file, and then the
//! directory itself is fsynced.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::policy::Thresholds;

/// Current on-disk format version.
pub const CONFIG_VERSION: u32 = 1;

/// Configuration files larger than this are rejected without parsing.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

const CONFIG_MODE: u32 = 0o644;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// `None` means no limit: factory charging behaviour.
    pub charge_limit: Option<Thresholds>,
    pub adapter_enabled: bool,
    /// Hold the charge limit by bypassing the adapter when the SMC offers no
    /// charge-inhibit key. The Mac then runs from its battery while the
    /// limit is held, which costs battery cycles, so this is opt-in.
    pub adapter_fallback: bool,
    pub prevent_idle_sleep_when_charging: bool,
    pub disable_charging_before_sleep: bool,
    pub prevent_system_sleep_when_charging: bool,
    pub prevent_sleep_while_adapter_disabled: bool,
    pub allow_non_root_access: bool,
    pub control_magsafe_led: bool,
    /// Automatically discharge to the limit on battery when current charge is above the upper limit.
    pub discharge_to_limit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            charge_limit: None,
            adapter_enabled: true,
            adapter_fallback: false,
            prevent_idle_sleep_when_charging: true,
            disable_charging_before_sleep: true,
            prevent_system_sleep_when_charging: false,
            prevent_sleep_while_adapter_disabled: false,
            allow_non_root_access: true,
            control_magsafe_led: false,
            discharge_to_limit: true,
        }
    }
}

/// A boolean setting that can be changed with `wattbit set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigOption {
    /// Hold an idle-sleep assertion while charging toward the limit.
    PreventIdleSleep,
    /// Inhibit charging before system sleep while a limit is set.
    DisableChargingBeforeSleep,
    /// Hold a system-sleep assertion while charging toward the limit.
    PreventSystemSleep,
    /// Hold an idle-sleep assertion while the adapter is bypassed.
    PreventSleepAdapterDisabled,
    /// Let non-root users query the daemon.
    AllowNonRoot,
    /// Drive the MagSafe LED to reflect the limit.
    MagsafeLed,
    /// Hold the limit by bypassing the adapter when charge inhibit is unsupported.
    AdapterFallback,
    /// Automatically discharge to the limit on battery when charge is above the limit.
    DischargeToLimit,
}

impl Config {
    pub fn option(&self, option: ConfigOption) -> bool {
        match option {
            ConfigOption::PreventIdleSleep => self.prevent_idle_sleep_when_charging,
            ConfigOption::DisableChargingBeforeSleep => self.disable_charging_before_sleep,
            ConfigOption::PreventSystemSleep => self.prevent_system_sleep_when_charging,
            ConfigOption::PreventSleepAdapterDisabled => self.prevent_sleep_while_adapter_disabled,
            ConfigOption::AllowNonRoot => self.allow_non_root_access,
            ConfigOption::MagsafeLed => self.control_magsafe_led,
            ConfigOption::AdapterFallback => self.adapter_fallback,
            ConfigOption::DischargeToLimit => self.discharge_to_limit,
        }
    }

    pub fn set_option(&mut self, option: ConfigOption, value: bool) {
        let field = match option {
            ConfigOption::PreventIdleSleep => &mut self.prevent_idle_sleep_when_charging,
            ConfigOption::DisableChargingBeforeSleep => &mut self.disable_charging_before_sleep,
            ConfigOption::PreventSystemSleep => &mut self.prevent_system_sleep_when_charging,
            ConfigOption::PreventSleepAdapterDisabled => {
                &mut self.prevent_sleep_while_adapter_disabled
            }
            ConfigOption::AllowNonRoot => &mut self.allow_non_root_access,
            ConfigOption::MagsafeLed => &mut self.control_magsafe_led,
            ConfigOption::AdapterFallback => &mut self.adapter_fallback,
            ConfigOption::DischargeToLimit => &mut self.discharge_to_limit,
        };
        *field = value;
    }

    /// Checks invariants serde cannot express. Threshold ranges are already
    /// enforced while deserializing.
    pub fn validate(&self) -> Result<(), String> {
        if self.version == 0 || self.version > CONFIG_VERSION {
            return Err(format!(
                "unsupported config version {} (this wattbit understands {CONFIG_VERSION})",
                self.version
            ));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("{} is not valid wattbit configuration: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{}: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads and validates the configuration. Returns `Ok(None)` when the
    /// file does not exist.
    pub fn load(&self) -> Result<Option<Config>, ConfigError> {
        let read_err = |source| ConfigError::Read {
            path: self.path.clone(),
            source,
        };
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(read_err(e)),
        };
        let mut contents = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut contents)
            .map_err(read_err)?;
        if contents.len() as u64 > MAX_CONFIG_BYTES {
            return Err(ConfigError::Invalid {
                path: self.path.clone(),
                reason: format!("file exceeds {MAX_CONFIG_BYTES} bytes"),
            });
        }
        let config: Config =
            serde_json::from_slice(&contents).map_err(|source| ConfigError::Parse {
                path: self.path.clone(),
                source,
            })?;
        config.validate().map_err(|reason| ConfigError::Invalid {
            path: self.path.clone(),
            reason,
        })?;
        Ok(Some(config))
    }

    /// Atomically replaces the configuration file.
    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        let write_err = |source| ConfigError::Write {
            path: self.path.clone(),
            source,
        };
        let mut json = serde_json::to_vec_pretty(config).map_err(|e| write_err(e.into()))?;
        json.push(b'\n');

        let dir = match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let file_name = self.path.file_name().ok_or_else(|| {
            write_err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path has no file name",
            ))
        })?;
        let tmp = dir.join(format!(
            ".{}.tmp.{}",
            file_name.to_string_lossy(),
            std::process::id()
        ));

        let result = (|| -> io::Result<()> {
            match fs::remove_file(&tmp) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(CONFIG_MODE)
                .open(&tmp)?;
            file.set_permissions(Permissions::from_mode(CONFIG_MODE))?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            File::open(&dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(write_err)
    }
}
