//! The daemon's control loop: reads telemetry, runs the charge policy and
//! reconciles hardware with the configuration.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::Mutex as AsyncMutex;

use crate::battery::BatteryTelemetry;
use crate::capabilities::{Capabilities, ChargeControlMode};
use crate::config::{Config, ConfigOption, ConfigStore};
use crate::conflicts::{self, Conflict};
use crate::hardware::{Hardware, HardwareError};
use crate::policy::{adapter_must_be_restored, ChargeLimiter, Thresholds, ADAPTER_SAFETY_FLOOR};
use crate::power::{AssertionKind, PowerAssertion, PowerEvent};
use crate::protocol::{
    Call, ChargingStatus, DaemonDiagnostics, ErrorKind, Reply, RpcError, StatusReport,
};
use crate::smc::keys::{LedMode, BCLM_MAX};

/// Upper bound on any single hardware operation.
pub const HARDWARE_TIMEOUT: Duration = Duration::from_secs(5);

/// Status requests reuse telemetry younger than this instead of re-reading.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Power-source notifications closer together than this are coalesced.
const EVENT_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Hardware(String),
    #[error("hardware call timed out after {} s", HARDWARE_TIMEOUT.as_secs())]
    Timeout,
    #[error("hardware task panicked")]
    Panicked,
    #[error("read-only mode: not applying {0}")]
    ReadOnly(&'static str),
}

impl From<HardwareError> for ControlError {
    fn from(e: HardwareError) -> Self {
        match e {
            HardwareError::Unsupported(reason) => ControlError::Unsupported(reason),
            other => ControlError::Hardware(other.to_string()),
        }
    }
}

impl From<ControlError> for RpcError {
    fn from(e: ControlError) -> Self {
        let kind = match e {
            ControlError::Unsupported(_) => ErrorKind::Unsupported,
            ControlError::Timeout => ErrorKind::Timeout,
            ControlError::Hardware(_) | ControlError::ReadOnly(_) => ErrorKind::Hardware,
            ControlError::Panicked => ErrorKind::Internal,
        };
        RpcError::new(kind, e.to_string())
    }
}

#[derive(Debug, Default)]
struct Assertions {
    charging_idle: Option<PowerAssertion>,
    charging_system: Option<PowerAssertion>,
    adapter_idle: Option<PowerAssertion>,
}

#[derive(Debug)]
struct State {
    config: Config,
    config_warning: Option<String>,
    limiter: ChargeLimiter,
    battery: Option<BatteryTelemetry>,
    battery_error: Option<String>,
    charging_allowed: Option<bool>,
    adapter_enabled: Option<bool>,
    last_error: Option<String>,
    last_refresh: Option<Instant>,
    /// Between a will-sleep notification and the following wake.
    asleep: bool,
    assertions: Assertions,
    /// LED mode berry last wrote; `None` while the system owns the LED.
    led: Option<LedMode>,
    /// Other charge limiters seen in the latest scan.
    conflicts: Vec<Conflict>,
    /// berry itself inhibited charging, as opposed to another tool.
    owns_inhibit: bool,
    /// berry itself bypassed the adapter, as opposed to another tool.
    owns_adapter_cut: bool,
}

#[derive(Debug)]
pub struct Controller {
    hw: Arc<Mutex<Hardware>>,
    caps: Capabilities,
    store: ConfigStore,
    socket_path: PathBuf,
    read_only: bool,
    state: AsyncMutex<State>,
}

impl Controller {
    pub fn new(
        hardware: Hardware,
        store: ConfigStore,
        config: Config,
        config_warning: Option<String>,
        socket_path: PathBuf,
        read_only: bool,
    ) -> Self {
        let caps = hardware.capabilities().clone();
        Self {
            hw: Arc::new(Mutex::new(hardware)),
            caps,
            store,
            socket_path,
            read_only,
            state: AsyncMutex::new(State {
                limiter: ChargeLimiter::new(config.charge_limit),
                config,
                config_warning,
                battery: None,
                battery_error: None,
                charging_allowed: None,
                adapter_enabled: None,
                last_error: None,
                last_refresh: None,
                asleep: false,
                assertions: Assertions::default(),
                led: None,
                conflicts: Vec::new(),
                owns_inhibit: false,
                owns_adapter_cut: false,
            }),
        }
    }

    pub async fn allow_non_root_access(&self) -> bool {
        self.state.lock().await.config.allow_non_root_access
    }

    /// Runs `f` against the hardware on the blocking pool, bounded by
    /// [`HARDWARE_TIMEOUT`].
    async fn hw<T, F>(&self, f: F) -> Result<T, ControlError>
    where
        T: Send + 'static,
        F: FnOnce(&Hardware) -> Result<T, HardwareError> + Send + 'static,
    {
        let hw = Arc::clone(&self.hw);
        let task = tokio::task::spawn_blocking(move || {
            let guard = hw.lock().unwrap_or_else(PoisonError::into_inner);
            f(&guard)
        });
        match tokio::time::timeout(HARDWARE_TIMEOUT, task).await {
            Ok(Ok(result)) => result.map_err(ControlError::from),
            Ok(Err(_)) => Err(ControlError::Panicked),
            Err(_) => Err(ControlError::Timeout),
        }
    }

    async fn write<F>(&self, what: &'static str, f: F) -> Result<(), ControlError>
    where
        F: FnOnce(&Hardware) -> Result<(), HardwareError> + Send + 'static,
    {
        if self.read_only {
            return Err(ControlError::ReadOnly(what));
        }
        self.hw(f).await
    }

    /// Drives charge inhibit to `inhibit` (only reads when `may_write` is
    /// false). Returns the resulting hardware state, or `None` if it could
    /// not be read.
    async fn ensure_charge_inhibit(
        &self,
        inhibit: bool,
        may_write: bool,
        errors: &mut Vec<String>,
    ) -> Option<bool> {
        let current = match self.hw(|hw| hw.charging_inhibited()).await {
            Ok(current) => current,
            Err(e) => {
                errors.push(format!("charge inhibit: {e}"));
                return None;
            }
        };
        if current == inhibit || !may_write {
            return Some(current);
        }
        match self
            .write("charge inhibit", move |hw| {
                hw.set_charging_inhibited(inhibit)
            })
            .await
        {
            Ok(()) => {
                log!("charging {}", if inhibit { "inhibited" } else { "allowed" });
                Some(inhibit)
            }
            Err(e) => {
                errors.push(format!("charge inhibit: {e}"));
                Some(current)
            }
        }
    }

    /// Drives the adapter to `enabled` (only reads when `may_write` is
    /// false). Returns the resulting hardware state, or `None` if it could
    /// not be read.
    async fn ensure_adapter(
        &self,
        enabled: bool,
        may_write: bool,
        errors: &mut Vec<String>,
    ) -> Option<bool> {
        let current = match self.hw(|hw| hw.adapter_disabled()).await {
            Ok(disabled) => !disabled,
            Err(e) => {
                errors.push(format!("adapter: {e}"));
                return None;
            }
        };
        if current == enabled || !may_write {
            return Some(current);
        }
        match self
            .write("adapter state", move |hw| hw.set_adapter_disabled(!enabled))
            .await
        {
            Ok(()) => {
                log!(
                    "power adapter {}",
                    if enabled { "enabled" } else { "disabled" }
                );
                Some(enabled)
            }
            Err(e) => {
                errors.push(format!("adapter: {e}"));
                Some(current)
            }
        }
    }

    async fn ensure_firmware_limit(
        &self,
        percent: u8,
        may_write: bool,
        errors: &mut Vec<String>,
    ) -> Option<u8> {
        let current = match self.hw(|hw| hw.firmware_limit()).await {
            Ok(current) => current,
            Err(e) => {
                errors.push(format!("BCLM: {e}"));
                return None;
            }
        };
        if current == percent || !may_write {
            return Some(current);
        }
        match self
            .write("BCLM", move |hw| hw.set_firmware_limit(percent))
            .await
        {
            Ok(()) => {
                log!("firmware charge limit set to {percent}%");
                Some(percent)
            }
            Err(e) => {
                errors.push(format!("BCLM: {e}"));
                Some(current)
            }
        }
    }

    /// Re-scans for other charge limiters, logging when the set changes.
    async fn refresh_conflicts(&self, st: &mut State) {
        let scan = tokio::time::timeout(
            HARDWARE_TIMEOUT,
            tokio::task::spawn_blocking(conflicts::running),
        )
        .await;
        let found = match scan {
            Ok(Ok(found)) => found,
            _ => {
                log!("scanning for other charge limiters failed; keeping the previous result");
                return;
            }
        };
        let before = conflicts::summarize(&st.conflicts);
        let after = conflicts::summarize(&found);
        if before != after {
            if found.is_empty() {
                log!("no other charge limiter is running; resuming hardware control");
            } else {
                log!(
                    "pausing hardware control: {after} also {} charging",
                    conflicts::agree(&found, "controls", "control")
                );
            }
        }
        st.conflicts = found;
    }

    fn conflict_error(st: &State) -> Result<(), RpcError> {
        if st.conflicts.is_empty() {
            return Ok(());
        }
        let remedies: Vec<String> = {
            let mut seen = Vec::new();
            for c in &st.conflicts {
                let line = format!("  {}: {}", c.tool, c.remedy);
                if !seen.contains(&line) {
                    seen.push(line);
                }
            }
            seen
        };
        Err(RpcError::new(
            ErrorKind::Conflict,
            format!(
                "other charge limiters are controlling the same hardware: {}. berry will not \
                 fight them. Stop and remove them first:\n{}",
                conflicts::summarize(&st.conflicts),
                remedies.join("\n")
            ),
        ))
    }

    pub async fn reconcile(&self) {
        let mut state = self.state.lock().await;
        self.reconcile_locked(&mut state).await;
    }

    async fn reconcile_locked(&self, st: &mut State) {
        let mut errors = Vec::new();

        // While another charge limiter runs, berry only observes; the
        // status report carries the conflicts.
        self.refresh_conflicts(st).await;
        let paused = !st.conflicts.is_empty();
        let may_write = !paused;

        match self.hw(|hw| Ok(hw.battery())).await {
            Ok(Ok(battery)) => {
                st.battery = Some(battery);
                st.battery_error = None;
            }
            Ok(Err(e)) => {
                st.battery = None;
                st.battery_error = Some(e.to_string());
            }
            Err(e) => errors.push(format!("battery: {e}")),
        }
        let charge = st.battery.as_ref().map(|b| b.charge_percent);
        let on_ac = st.battery.as_ref().is_some_and(BatteryTelemetry::on_ac);
        let is_charging = st.battery.as_ref().is_some_and(|b| b.is_charging);

        st.limiter.set_thresholds(st.config.charge_limit);
        let previous = st.limiter.state();
        if let Some(charge) = charge {
            st.limiter.update(charge);
        }
        if let (Some(state), Some(charge)) = (st.limiter.state(), charge) {
            if Some(state) != previous {
                log!("policy: {state:?} at {charge}%");
            }
        }
        let limit_active = st.config.charge_limit.is_some();
        let policy_allows = st.limiter.allows_charging();
        let mode = self.caps.charge_control_mode(st.config.adapter_fallback);

        // A user-disabled adapter is re-enabled at the safety floor.
        if !st.config.adapter_enabled
            && charge.is_some_and(adapter_must_be_restored)
            && self.caps.adapter_control.is_usable()
        {
            log!("battery at or below {ADAPTER_SAFETY_FLOOR}%: re-enabling the power adapter");
            let mut next = st.config.clone();
            next.adapter_enabled = true;
            if let Err(e) = self.store.save(&next) {
                errors.push(format!("saving configuration: {e}"));
            }
            st.config = next;
        }

        st.charging_allowed = None;
        match mode {
            ChargeControlMode::Native => {
                let hold_for_sleep = st.asleep && st.config.disable_charging_before_sleep;
                let inhibit = limit_active && (!policy_allows || hold_for_sleep);
                let state = self
                    .ensure_charge_inhibit(inhibit, may_write, &mut errors)
                    .await;
                if may_write {
                    st.owns_inhibit = inhibit && state == Some(true);
                }
                st.charging_allowed = state.map(|inhibited| !inhibited);
            }
            ChargeControlMode::FirmwareLimit => {
                let target = st.config.charge_limit.map_or(BCLM_MAX, Thresholds::upper);
                if let Some(limit) = self
                    .ensure_firmware_limit(target, may_write, &mut errors)
                    .await
                {
                    st.charging_allowed = charge.map(|c| c < limit);
                }
            }
            ChargeControlMode::AdapterFallback => {}
            ChargeControlMode::Unsupported => {
                if limit_active {
                    errors
                        .push("a charge limit is configured but this Mac cannot enforce it".into());
                }
            }
        }

        if self.caps.adapter_control.is_usable() {
            let mut want_enabled = st.config.adapter_enabled;
            // The fallback bypass is never held across sleep: the daemon
            // cannot watch the safety floor while the system sleeps.
            if mode == ChargeControlMode::AdapterFallback
                && limit_active
                && !policy_allows
                && !st.asleep
            {
                want_enabled = false;
            }
            if charge.is_some_and(adapter_must_be_restored) {
                want_enabled = true;
            }
            st.adapter_enabled = self
                .ensure_adapter(want_enabled, may_write, &mut errors)
                .await;
            if may_write {
                st.owns_adapter_cut = !want_enabled && st.adapter_enabled == Some(false);
            }
            match mode {
                ChargeControlMode::AdapterFallback => st.charging_allowed = st.adapter_enabled,
                ChargeControlMode::Unsupported if st.adapter_enabled == Some(false) => {
                    st.charging_allowed = Some(false)
                }
                _ => {}
            }
        } else {
            st.adapter_enabled = None;
        }

        let assertions_allowed = !self.read_only && !paused;
        let charging_toward_limit =
            limit_active && policy_allows && on_ac && mode != ChargeControlMode::Unsupported;
        sync_assertion(
            &mut st.assertions.charging_idle,
            assertions_allowed
                && st.config.prevent_idle_sleep_when_charging
                && charging_toward_limit,
            AssertionKind::PreventUserIdleSystemSleep,
            "berry: charging toward the charge limit",
            &mut errors,
        );
        sync_assertion(
            &mut st.assertions.charging_system,
            assertions_allowed
                && st.config.prevent_system_sleep_when_charging
                && charging_toward_limit,
            AssertionKind::PreventSystemSleep,
            "berry: charging toward the charge limit",
            &mut errors,
        );
        sync_assertion(
            &mut st.assertions.adapter_idle,
            assertions_allowed
                && st.config.prevent_sleep_while_adapter_disabled
                && st.adapter_enabled == Some(false),
            AssertionKind::PreventUserIdleSystemSleep,
            "berry: power adapter disabled",
            &mut errors,
        );

        if self.caps.magsafe_led.is_usable() && may_write {
            let target = (st.config.control_magsafe_led && limit_active && on_ac).then_some(
                if is_charging {
                    LedMode::Orange
                } else {
                    LedMode::Green
                },
            );
            if target != st.led {
                let mode = target.unwrap_or(LedMode::System);
                match self.write("MagSafe LED", move |hw| hw.set_led(mode)).await {
                    Ok(()) => st.led = target,
                    Err(e) => errors.push(format!("MagSafe LED: {e}")),
                }
            }
        }

        let error = (!errors.is_empty()).then(|| errors.join("; "));
        if error != st.last_error {
            match &error {
                Some(e) => log!("{e}"),
                None => log!("hardware state reconciled"),
            }
        }
        st.last_error = error;
        st.last_refresh = Some(Instant::now());
    }

    pub async fn handle_power_event(&self, event: PowerEvent) {
        let mut st = self.state.lock().await;
        match event {
            PowerEvent::WillSleep { ack } => {
                st.asleep = true;
                self.reconcile_locked(&mut st).await;
                let _ = ack.send(());
            }
            PowerEvent::HasPoweredOn => {
                st.asleep = false;
                self.reconcile_locked(&mut st).await;
            }
            PowerEvent::PowerSourceChanged => {
                if st
                    .last_refresh
                    .is_none_or(|t| t.elapsed() >= EVENT_DEBOUNCE)
                {
                    self.reconcile_locked(&mut st).await;
                }
            }
        }
    }

    /// Restores factory behaviour before the daemon exits. While another
    /// charge limiter runs, only state berry itself set is undone.
    pub async fn shutdown(&self) {
        let mut st = self.state.lock().await;
        st.assertions = Assertions::default();
        if self.read_only {
            return;
        }
        let foreign = !st.conflicts.is_empty();
        let mut errors = Vec::new();
        if self.caps.charge_inhibit.is_usable() && (!foreign || st.owns_inhibit) {
            self.ensure_charge_inhibit(false, true, &mut errors).await;
        }
        if self.caps.adapter_control.is_usable() && (!foreign || st.owns_adapter_cut) {
            self.ensure_adapter(true, true, &mut errors).await;
        }
        if st.led.take().is_some() {
            if let Err(e) = self
                .write("MagSafe LED", |hw| hw.set_led(LedMode::System))
                .await
            {
                errors.push(format!("MagSafe LED: {e}"));
            }
        }
        if !errors.is_empty() {
            log!(
                "restoring factory charging behaviour failed: {}",
                errors.join("; ")
            );
        } else if foreign {
            log!("undid berry's own changes; other charge limiters keep control");
        } else {
            log!("factory charging behaviour restored");
        }
    }

    pub async fn handle_call(&self, call: Call) -> Result<Reply, RpcError> {
        match call {
            Call::Status => Ok(Reply::Status(Box::new(self.status().await))),
            Call::Doctor => Ok(Reply::Diagnostics(Box::new(self.diagnostics().await))),
            Call::SetLimit { upper, lower } => self.set_limit(upper, lower).await,
            Call::DisableLimit => self.disable_limit().await,
            Call::SetAdapter { enabled } => self.set_adapter(enabled).await,
            Call::SetOption { option, enabled } => self.set_option(option, enabled).await,
        }
    }

    async fn status(&self) -> StatusReport {
        let mut st = self.state.lock().await;
        if st
            .last_refresh
            .is_none_or(|t| t.elapsed() >= MIN_REFRESH_INTERVAL)
        {
            self.reconcile_locked(&mut st).await;
        }
        StatusReport {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            battery: st.battery.clone(),
            battery_error: st.battery_error.clone(),
            charging: ChargingStatus {
                charging_allowed: st.charging_allowed,
                policy_state: st.limiter.state(),
                adapter_enabled: st.adapter_enabled,
                last_error: st.last_error.clone(),
            },
            config: st.config.clone(),
            config_warning: st.config_warning.clone(),
            mode: self.caps.charge_control_mode(st.config.adapter_fallback),
            capabilities: self.caps.clone(),
            conflicts: st.conflicts.clone(),
        }
    }

    async fn diagnostics(&self) -> DaemonDiagnostics {
        let st = self.state.lock().await;
        DaemonDiagnostics {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            // SAFETY: geteuid has no preconditions.
            euid: unsafe { libc::geteuid() },
            socket_path: self.socket_path.display().to_string(),
            config_path: self.store.path().display().to_string(),
            config_warning: st.config_warning.clone(),
            mode: self.caps.charge_control_mode(st.config.adapter_fallback),
            capabilities: self.caps.clone(),
            last_error: st.last_error.clone(),
            conflicts: st.conflicts.clone(),
        }
    }

    fn commit(&self, st: &mut State, next: Config) -> Result<(), RpcError> {
        self.store.save(&next).map_err(|e| {
            RpcError::new(
                ErrorKind::Internal,
                format!("cannot save configuration: {e}"),
            )
        })?;
        st.config = next;
        st.config_warning = None;
        Ok(())
    }

    /// Fails with the reconcile error, if any, noting the saved change.
    fn applied(st: &State, what: &str) -> Result<(), RpcError> {
        match &st.last_error {
            Some(e) => Err(RpcError::new(
                ErrorKind::Hardware,
                format!("{what} was saved, but applying it failed: {e}"),
            )),
            None => Ok(()),
        }
    }

    fn unsupported_limit_reason(&self) -> String {
        let mut message = format!(
            "this Mac exposes no usable charge-inhibit control ({})",
            self.caps
                .charge_inhibit
                .reason()
                .unwrap_or("unknown reason")
        );
        if self.caps.adapter_control.is_usable() {
            message.push_str(
                ". Adapter control is available: `sudo berry set adapter-fallback on` holds the \
                 limit by running from the battery instead (this costs battery cycles)",
            );
        }
        message
    }

    async fn set_limit(&self, upper: u8, lower: u8) -> Result<Reply, RpcError> {
        let thresholds = Thresholds::new(upper, Some(lower))
            .map_err(|e| RpcError::new(ErrorKind::InvalidParams, e.to_string()))?;
        let mut st = self.state.lock().await;
        let mode = self.caps.charge_control_mode(st.config.adapter_fallback);
        if mode == ChargeControlMode::Unsupported {
            return Err(RpcError::new(
                ErrorKind::Unsupported,
                self.unsupported_limit_reason(),
            ));
        }
        self.refresh_conflicts(&mut st).await;
        Self::conflict_error(&st)?;
        let mut next = st.config.clone();
        next.charge_limit = Some(thresholds);
        self.commit(&mut st, next)?;
        log!("charge limit set to {upper}% / {lower}%");
        self.reconcile_locked(&mut st).await;
        Self::applied(&st, "the charge limit")?;
        let mut message = format!(
            "Charge limit set: charging stops at {upper}% and resumes below {lower}% ({} mode).",
            mode.label()
        );
        match mode {
            ChargeControlMode::FirmwareLimit => message.push_str(
                " The firmware enforces the upper limit; the lower limit is not used in this mode.",
            ),
            ChargeControlMode::AdapterFallback => {
                message.push_str(" The Mac runs from its battery while the limit is held.")
            }
            _ => {}
        }
        Ok(Reply::Ack { message })
    }

    async fn disable_limit(&self) -> Result<Reply, RpcError> {
        let mut st = self.state.lock().await;
        let mut next = st.config.clone();
        next.charge_limit = None;
        self.commit(&mut st, next)?;
        log!("charge limit disabled");
        self.reconcile_locked(&mut st).await;
        if !st.conflicts.is_empty() {
            return Ok(Reply::Ack {
                message: format!(
                    "Charge limit disabled. Note: {} still {} charging.",
                    conflicts::summarize(&st.conflicts),
                    conflicts::agree(&st.conflicts, "controls", "control")
                ),
            });
        }
        Self::applied(&st, "disabling the charge limit")?;
        Ok(Reply::Ack {
            message: "Charge limit disabled; factory charging behaviour restored.".into(),
        })
    }

    async fn set_adapter(&self, enabled: bool) -> Result<Reply, RpcError> {
        if !self.caps.adapter_control.is_usable() {
            return Err(RpcError::new(
                ErrorKind::Unsupported,
                format!(
                    "power adapter control is not supported on this Mac: {}",
                    self.caps
                        .adapter_control
                        .reason()
                        .unwrap_or("unknown reason")
                ),
            ));
        }
        let mut st = self.state.lock().await;
        self.refresh_conflicts(&mut st).await;
        Self::conflict_error(&st)?;
        if !enabled {
            if let Some(charge) = st.battery.as_ref().map(|b| b.charge_percent) {
                if adapter_must_be_restored(charge) {
                    return Err(RpcError::new(
                        ErrorKind::InvalidParams,
                        format!(
                            "battery is at {charge}%, at or below the {ADAPTER_SAFETY_FLOOR}% \
                             safety floor; refusing to disable the power adapter"
                        ),
                    ));
                }
            }
        }
        let mut next = st.config.clone();
        next.adapter_enabled = enabled;
        self.commit(&mut st, next)?;
        self.reconcile_locked(&mut st).await;
        Self::applied(&st, "the adapter setting")?;
        let message = if enabled {
            "Power adapter enabled.".to_owned()
        } else {
            format!(
                "Power adapter disabled; the Mac now runs from its battery. berry re-enables \
                 the adapter automatically at {ADAPTER_SAFETY_FLOOR}%."
            )
        };
        Ok(Reply::Ack { message })
    }

    async fn set_option(&self, option: ConfigOption, enabled: bool) -> Result<Reply, RpcError> {
        if enabled {
            let required = match option {
                ConfigOption::MagsafeLed => Some(("MagSafe LED control", &self.caps.magsafe_led)),
                ConfigOption::AdapterFallback => {
                    Some(("adapter control", &self.caps.adapter_control))
                }
                _ => None,
            };
            if let Some((name, support)) = required {
                if !support.is_usable() {
                    return Err(RpcError::new(
                        ErrorKind::Unsupported,
                        format!(
                            "{name} is not supported on this Mac: {}",
                            support.reason().unwrap_or("unknown reason")
                        ),
                    ));
                }
            }
        }
        let mut st = self.state.lock().await;
        let mut next = st.config.clone();
        next.set_option(option, enabled);
        self.commit(&mut st, next)?;
        if option == ConfigOption::AllowNonRoot {
            super::set_socket_permissions(&self.socket_path, enabled).map_err(|e| {
                RpcError::new(
                    ErrorKind::Internal,
                    format!("cannot update socket permissions: {e}"),
                )
            })?;
        }
        self.reconcile_locked(&mut st).await;
        Ok(Reply::Ack {
            message: format!(
                "{} {}.",
                option_label(option),
                if enabled { "enabled" } else { "disabled" }
            ),
        })
    }
}

fn option_label(option: ConfigOption) -> &'static str {
    match option {
        ConfigOption::PreventIdleSleep => "Prevent idle-sleep when charging",
        ConfigOption::DisableChargingBeforeSleep => "Disable charging before sleep",
        ConfigOption::PreventSystemSleep => "Prevent system-sleep when charging",
        ConfigOption::PreventSleepAdapterDisabled => "Prevent sleep while adapter is disabled",
        ConfigOption::AllowNonRoot => "Non-root daemon access",
        ConfigOption::MagsafeLed => "MagSafe LED control",
        ConfigOption::AdapterFallback => "Adapter fallback charge limiting",
    }
}

fn sync_assertion(
    slot: &mut Option<PowerAssertion>,
    wanted: bool,
    kind: AssertionKind,
    reason: &str,
    errors: &mut Vec<String>,
) {
    match (wanted, slot.is_some()) {
        (true, false) => match PowerAssertion::acquire(kind, reason) {
            Ok(assertion) => *slot = Some(assertion),
            Err(e) => errors.push(format!("power assertion: {e}")),
        },
        (false, true) => *slot = None,
        _ => {}
    }
}
