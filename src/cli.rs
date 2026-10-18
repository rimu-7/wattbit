//! Command-line interface definitions.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::ConfigOption;
use crate::policy::{Thresholds, DEFAULT_UPPER, MAX_UPPER, MIN_UPPER};
use crate::protocol::Call;
use crate::speed::agent::{DEFAULT_INTERVAL_SECONDS, MAX_INTERVAL_SECONDS, MIN_INTERVAL_SECONDS};
use crate::{CONFIG_PATH, SOCKET_PATH};

#[derive(Debug, Parser)]
#[command(
    name = "berry",
    version,
    about = "Battery charge limiting, power adapter control and battery telemetry for macOS",
    propagate_version = true
)]
pub struct Cli {
    /// Path of the daemon's Unix socket.
    #[arg(long, global = true, value_name = "PATH", default_value = SOCKET_PATH)]
    pub socket: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show charging, battery, configuration and hardware status.
    Status {
        /// Print the raw status report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Set the charge limit (default 80%, resuming below 75%), or `disable` it.
    Limit(LimitArgs),
    /// Use (`enable`) or bypass (`disable`) the power adapter.
    Adapter {
        #[arg(value_enum)]
        action: Toggle,
    },
    /// Turn a daemon setting on or off.
    Set {
        #[arg(value_enum)]
        option: ConfigOption,
        #[arg(value_enum)]
        value: Switch,
    },
    /// Run diagnostic checks on the hardware, daemon and configuration.
    Doctor,
    /// Show live internet speeds in the menu bar and record usage by day.
    Speed {
        #[command(subcommand)]
        command: SpeedCommand,
    },
    /// Run or manage the daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Install the daemon as a LaunchDaemon and start it (requires root).
    InstallService,
    /// Stop and remove the LaunchDaemon, restoring factory charging (requires root).
    UninstallService {
        /// Also delete the configuration file.
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Run the daemon in the foreground (this is what launchd runs).
    Run {
        /// Path of the configuration file.
        #[arg(long, value_name = "PATH", default_value = CONFIG_PATH)]
        config: PathBuf,
        /// Observe and report without writing to the hardware.
        #[arg(long)]
        read_only: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SpeedCommand {
    /// Show the speed meter in the menu bar and start recording usage.
    On {
        /// Run the meter without installing a login item; it stops at logout.
        #[arg(long)]
        no_launchd: bool,
    },
    /// Hide the meter and stop recording usage.
    Off,
    /// Show current speeds and internet usage for the last 31 days.
    Status {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run the sampling agent (this is what `berry speed on` starts).
    #[command(hide = true)]
    Agent {
        /// Record usage without creating the menu bar item.
        #[arg(long)]
        headless: bool,
        /// Seconds between samples.
        #[arg(
            long,
            value_name = "SECONDS",
            default_value_t = DEFAULT_INTERVAL_SECONDS,
            value_parser = parse_interval
        )]
        interval: f64,
        /// Take one sample, save it and exit.
        #[arg(long)]
        once: bool,
    },
}

/// Parses a sampling interval in seconds.
pub fn parse_interval(text: &str) -> Result<f64, String> {
    let range_error =
        || format!("expected seconds from {MIN_INTERVAL_SECONDS} to {MAX_INTERVAL_SECONDS}");
    let value: f64 = text.parse().map_err(|_| range_error())?;
    if !(MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&value) {
        return Err(range_error());
    }
    Ok(value)
}

#[derive(Debug, Args)]
pub struct LimitArgs {
    /// Upper limit in percent (50-100), or `disable`.
    #[arg(
        value_name = "PERCENT|disable",
        value_parser = parse_limit_target,
        default_value_t = LimitTarget::Percent(DEFAULT_UPPER)
    )]
    pub target: LimitTarget,

    /// Lower limit in percent; charging resumes below it. Defaults to five
    /// below the upper limit.
    #[arg(long, value_name = "PERCENT")]
    pub lower: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitTarget {
    Percent(u8),
    Disable,
}

impl std::fmt::Display for LimitTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitTarget::Percent(p) => write!(f, "{p}"),
            LimitTarget::Disable => f.write_str("disable"),
        }
    }
}

/// Parses `80`, `80%` or `disable`.
pub fn parse_limit_target(text: &str) -> Result<LimitTarget, String> {
    if text.eq_ignore_ascii_case("disable") {
        return Ok(LimitTarget::Disable);
    }
    let range_error =
        || format!("expected a percentage from {MIN_UPPER} to {MAX_UPPER}, or `disable`");
    let digits = text.strip_suffix('%').unwrap_or(text);
    let value: u8 = digits.parse().map_err(|_| range_error())?;
    if !(MIN_UPPER..=MAX_UPPER).contains(&value) {
        return Err(range_error());
    }
    Ok(LimitTarget::Percent(value))
}

/// A validated `berry limit` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitRequest {
    Set(Thresholds),
    Disable,
}

impl LimitArgs {
    pub fn resolve(&self) -> Result<LimitRequest, String> {
        match self.target {
            LimitTarget::Disable if self.lower.is_some() => {
                Err("--lower cannot be combined with `disable`".into())
            }
            LimitTarget::Disable => Ok(LimitRequest::Disable),
            LimitTarget::Percent(upper) => Thresholds::new(upper, self.lower)
                .map(LimitRequest::Set)
                .map_err(|e| e.to_string()),
        }
    }
}

impl LimitRequest {
    pub fn into_call(self) -> Call {
        match self {
            LimitRequest::Set(t) => Call::SetLimit {
                upper: t.upper(),
                lower: t.lower(),
            },
            LimitRequest::Disable => Call::DisableLimit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    Enable,
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}
