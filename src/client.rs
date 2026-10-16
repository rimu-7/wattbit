//! Socket client and the `berry status` renderer.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::capabilities::Support;
use crate::policy::ChargeState;
use crate::protocol::{
    encode_line, Call, Outcome, Reply, Request, Response, RpcError, StatusReport, CLIENT_TIMEOUT,
    MAX_MESSAGE_BYTES,
};

pub const CHECK: &str = "✔";
pub const CROSS: &str = "✘";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(
        "the berry daemon is not running (nothing is listening on {path}); \
         install it with `sudo berry install-service`"
    )]
    NotRunning { path: String },
    #[error(
        "permission denied connecting to {path}; non-root access may be disabled, re-run with sudo"
    )]
    PermissionDenied { path: String },
    #[error("cannot connect to {path}: {source}")]
    Connect { path: String, source: io::Error },
    #[error("the daemon did not answer within {} s", CLIENT_TIMEOUT.as_secs())]
    Timeout,
    #[error("I/O error talking to the daemon: {0}")]
    Io(#[from] io::Error),
    #[error("malformed reply from the daemon: {0}")]
    Protocol(String),
    #[error("{0}")]
    Rpc(RpcError),
}

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

const REQUEST_ID: u64 = 1;

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Sends one call and waits for its reply, bounded by [`CLIENT_TIMEOUT`].
    pub async fn call(&self, call: Call) -> Result<Reply, ClientError> {
        timeout(CLIENT_TIMEOUT, self.exchange(call))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn exchange(&self, call: Call) -> Result<Reply, ClientError> {
        let path = self.socket.display().to_string();
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                    ClientError::NotRunning { path: path.clone() }
                }
                io::ErrorKind::PermissionDenied => {
                    ClientError::PermissionDenied { path: path.clone() }
                }
                _ => ClientError::Connect {
                    path: path.clone(),
                    source: e,
                },
            })?;
        let (read_half, mut write_half) = stream.into_split();

        let request = encode_line(&Request::new(REQUEST_ID, call))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        write_half.write_all(&request).await?;
        write_half.shutdown().await?;

        let mut line = Vec::new();
        BufReader::new(read_half)
            .take(MAX_MESSAGE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if line.is_empty() {
            return Err(ClientError::Protocol(
                "connection closed without a reply".into(),
            ));
        }
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(ClientError::Protocol(format!(
                "reply exceeds {MAX_MESSAGE_BYTES} bytes"
            )));
        }
        let response: Response =
            serde_json::from_slice(&line).map_err(|e| ClientError::Protocol(e.to_string()))?;
        match response.outcome {
            Outcome::Error(error) => Err(ClientError::Rpc(error)),
            Outcome::Result(_) if response.id != Some(REQUEST_ID) => {
                Err(ClientError::Protocol(format!(
                    "reply id {:?} does not match request id {REQUEST_ID}",
                    response.id
                )))
            }
            Outcome::Result(reply) => Ok(reply),
        }
    }
}

fn mark(value: bool) -> &'static str {
    if value {
        CHECK
    } else {
        CROSS
    }
}

/// `✔` or `✘` for a known state, `unsupported` when berry cannot tell.
fn tri_state(value: Option<bool>) -> &'static str {
    value.map_or("unsupported", mark)
}

/// `✔` supported, `✔ (unverified)` usable but untested, `✘` unsupported.
pub fn support_mark(support: &Support) -> &'static str {
    match support {
        Support::Supported => CHECK,
        Support::Unverified { .. } => "✔ (unverified)",
        Support::Unsupported { .. } => CROSS,
    }
}

/// Renders the `berry status` dashboard.
pub fn render_status(report: &StatusReport) -> String {
    let mut out = String::new();
    let charging = &report.charging;
    let config = &report.config;

    out.push_str("Charging status:\n");
    let _ = writeln!(
        out,
        "  Allow charging: {} (refreshes can take up to 2 minutes)",
        tri_state(charging.charging_allowed)
    );
    if charging.charging_allowed == Some(false) {
        match (charging.policy_state, config.charge_limit) {
            (Some(ChargeState::ChargingSuppressed), _) => out.push_str(
                "    Your Mac will not charge, because your current charge is above the limit.\n",
            ),
            (Some(ChargeState::WaitingToResumeCharging), Some(limit)) => {
                let _ = writeln!(
                    out,
                    "    Your Mac will resume charging once the charge drops below {}%.",
                    limit.lower()
                );
            }
            _ => {}
        }
    }
    let _ = writeln!(
        out,
        "  Use power adapter: {}",
        tri_state(charging.adapter_enabled)
    );
    if let Some(error) = &charging.last_error {
        let _ = writeln!(out, "  Last error: {error}");
    }

    out.push_str("\nBattery status:\n");
    match &report.battery {
        Some(battery) => {
            let _ = writeln!(out, "  Current charge: {}%", battery.charge_percent);
            let _ = writeln!(out, "  State: {}", battery.activity().label());
            let capacity = match (battery.full_capacity_mah, battery.design_capacity_mah) {
                (Some(full), Some(design)) => match battery.health_percent() {
                    Some(health) => format!("{full} mAh / {design} mAh ({health}%)"),
                    None => format!("{full} mAh / {design} mAh"),
                },
                (Some(full), None) => format!("{full} mAh"),
                _ => "unavailable".to_owned(),
            };
            let _ = writeln!(out, "  Full capacity: {capacity}");
            let rate = match battery.power_watts() {
                // Avoid printing "-0.0 W".
                Some(w) if w.abs() < 0.05 => "+0.0 W".to_owned(),
                Some(w) => format!("{w:+.1} W"),
                None => "unavailable".to_owned(),
            };
            let _ = writeln!(out, "  Charge rate: {rate}");
            let voltage = battery.voltage_mv.map_or("unavailable".to_owned(), |mv| {
                format!("{:.2} V", f64::from(mv) / 1000.0)
            });
            let _ = writeln!(out, "  Voltage: {voltage}");
        }
        None => {
            let _ = writeln!(
                out,
                "  unavailable: {}",
                report.battery_error.as_deref().unwrap_or("no battery data")
            );
        }
    }

    out.push_str("\nBattery configuration:\n");
    match config.charge_limit {
        Some(limit) => {
            let _ = writeln!(out, "  Upper limit: {}%", limit.upper());
            let _ = writeln!(out, "  Lower limit: {}%", limit.lower());
        }
        None => out.push_str("  Upper limit: disabled\n  Lower limit: disabled\n"),
    }
    let options = [
        (
            "Prevent idle-sleep when charging",
            config.prevent_idle_sleep_when_charging,
        ),
        (
            "Disable charging before sleep if charge limit is enabled",
            config.disable_charging_before_sleep,
        ),
        (
            "Prevent system-sleep when charging",
            config.prevent_system_sleep_when_charging,
        ),
        (
            "Prevent sleep while adapter is disabled",
            config.prevent_sleep_while_adapter_disabled,
        ),
        (
            "Allow non-root users to access the daemon",
            config.allow_non_root_access,
        ),
        ("Control MagSafe LED", config.control_magsafe_led),
    ];
    for (label, value) in options {
        let _ = writeln!(out, "  {label}: {}", mark(value));
    }

    let caps = &report.capabilities;
    out.push_str("\nHardware compatibility:\n");
    let _ = writeln!(out, "  Charge control mode: {}", report.mode.label());
    let _ = writeln!(out, "  Sleep hooks: {}", support_mark(&caps.sleep_hooks));
    let _ = writeln!(
        out,
        "  MagSafe LED control: {}",
        support_mark(&caps.magsafe_led)
    );
    let _ = writeln!(
        out,
        "  Power adapter control: {}",
        support_mark(&caps.adapter_control)
    );
    let _ = writeln!(
        out,
        "  Auto calibration: {}",
        support_mark(&caps.calibration)
    );

    // berry does not implement calibration, so none is ever running or
    // scheduled.
    out.push_str("\nCalibration status:\n  Phase: Idle\n  Schedule: disabled\n");

    if let Some(warning) = &report.config_warning {
        let _ = write!(out, "\nWarning: {warning}\n");
    }
    out
}
