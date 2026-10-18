//! `wattbit doctor`: diagnostic checks.
//!
//! Local checks run as the invoking user and never write to the SMC. Write
//! access is reported from the daemon, which tests it as root at startup.

use std::fmt::Write as _;
use std::path::Path;

use crate::battery::{self, BatteryError};
use crate::capabilities::{self, Capabilities, Support};
use crate::client::{Client, ClientError};
use crate::config::ConfigStore;
use crate::conflicts::{self, Conflict};
use crate::protocol::{Call, DaemonDiagnostics, Reply};
use crate::service::{INSTALLED_BINARY, PLIST_PATH};
use crate::smc::Smc;
use crate::speed::agent::{self, SpeedPaths};
use crate::update::{self, INSTALL_COMMAND};
use crate::{legacy, CONFIG_PATH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Pass,
    Info,
    Warn,
    Fail,
}

impl Level {
    fn symbol(self) -> &'static str {
        match self {
            Level::Pass => "✔",
            Level::Info => "•",
            Level::Warn => "!",
            Level::Fail => "✘",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub name: String,
    pub detail: String,
}

fn check(level: Level, name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        level,
        name: name.into(),
        detail: detail.into(),
    }
}

/// `smc_control` marks features whose support is established by an SMC
/// write test, as opposed to OS facilities such as sleep hooks.
fn support_check(name: &str, support: &Support, smc_control: bool) -> Check {
    match support {
        Support::Supported if smc_control => {
            check(Level::Pass, name, "supported (write access verified)")
        }
        Support::Supported => check(Level::Pass, name, "supported"),
        Support::Unverified { reason } => check(Level::Warn, name, format!("unverified: {reason}")),
        Support::Unsupported { reason } => {
            check(Level::Warn, name, format!("unsupported: {reason}"))
        }
    }
}

/// Runs every check. Never fails; problems are reported as checks.
pub async fn run(socket: &Path) -> Vec<Check> {
    let mut checks = Vec::new();

    let host = capabilities::host_info();
    let mut detail = format!(
        "{}, {}, macOS {}",
        host.arch,
        host.model.as_deref().unwrap_or("unknown model"),
        host.os_version.as_deref().unwrap_or("unknown")
    );
    let level = if host.translated {
        detail.push_str("; wattbit is running under Rosetta 2, install the arm64 build");
        Level::Warn
    } else {
        Level::Pass
    };
    checks.push(check(level, "Architecture", detail));
    let speed = SpeedPaths::resolve().ok();
    checks.push(
        match speed.as_ref().and_then(|paths| update::pending(&paths.dir)) {
            Some(notice) => check(
                Level::Warn,
                "Version",
                format!(
                    "wattbit {} (wattbit {} is available). Update with:\n    {}",
                    notice.current, notice.latest, notice.command
                ),
            ),
            None => check(
                Level::Pass,
                "Version",
                format!("wattbit {}", env!("CARGO_PKG_VERSION")),
            ),
        },
    );

    match battery::read() {
        Ok(b) => {
            let mut detail = format!("{}%, {}", b.charge_percent, b.activity().label());
            if let Some(cycles) = b.cycle_count {
                let _ = write!(detail, ", {cycles} cycles");
            }
            if let Some(health) = b.health_percent() {
                let _ = write!(detail, ", {health}% of design capacity");
            }
            checks.push(check(Level::Pass, "Battery", detail));
        }
        Err(BatteryError::NoBattery) => checks.push(check(
            Level::Warn,
            "Battery",
            "no internal battery; nothing to manage",
        )),
        Err(e) => checks.push(check(Level::Fail, "Battery", e.to_string())),
    }

    let smc = Smc::open();
    match &smc {
        Ok(smc) => {
            let detail = match smc.key_count() {
                Ok(count) => format!("AppleSMC user client open, {count} keys"),
                Err(_) => "AppleSMC user client open".to_owned(),
            };
            checks.push(check(Level::Pass, "SMC connection", detail));
        }
        Err(e) => checks.push(check(Level::Fail, "SMC connection", e.to_string())),
    }
    let local = capabilities::detect(smc.as_ref(), false).capabilities;

    let daemon = Client::new(socket).call(Call::Doctor).await;
    let diagnostics = match daemon {
        Ok(Reply::Diagnostics(d)) => {
            let level = if d.euid == 0 {
                Level::Pass
            } else {
                Level::Warn
            };
            let mut detail = format!(
                "wattbitd {} answering on {} (pid {}, uid {})",
                d.version, d.socket_path, d.pid, d.euid
            );
            if d.euid != 0 {
                detail.push_str("; not root, so hardware writes are disabled");
            }
            checks.push(check(level, "Daemon socket", detail));
            if d.version != env!("CARGO_PKG_VERSION") {
                checks.push(check(
                    Level::Warn,
                    "Daemon version",
                    format!(
                        "the daemon runs wattbit {}, but this command is wattbit {}. \
                         Finish the update with:\n    {INSTALL_COMMAND}",
                        d.version,
                        env!("CARGO_PKG_VERSION")
                    ),
                ));
            }
            if let Some(error) = &d.last_error {
                checks.push(check(Level::Warn, "Daemon state", error.clone()));
            }
            Some(d)
        }
        Ok(_) => {
            checks.push(check(
                Level::Fail,
                "Daemon socket",
                "unexpected reply to `doctor`",
            ));
            None
        }
        Err(ClientError::NotRunning { path }) => {
            checks.push(check(
                Level::Fail,
                "Daemon socket",
                format!("nothing is listening on {path}"),
            ));
            None
        }
        Err(e) => {
            checks.push(check(Level::Fail, "Daemon socket", e.to_string()));
            None
        }
    };

    conflict_checks(&mut checks, diagnostics.as_deref());
    key_checks(&mut checks, &local, diagnostics.as_deref());

    let caps = diagnostics.as_ref().map_or(&local, |d| &d.capabilities);
    if let Some(d) = &diagnostics {
        checks.push(check(Level::Info, "Charge control mode", d.mode.label()));
    }
    checks.push(support_check("Charge inhibit", &caps.charge_inhibit, true));
    checks.push(support_check(
        "Firmware limit (BCLM)",
        &caps.firmware_limit,
        true,
    ));
    checks.push(support_check(
        "Power adapter control",
        &caps.adapter_control,
        true,
    ));
    checks.push(support_check(
        "MagSafe LED control",
        &caps.magsafe_led,
        true,
    ));
    checks.push(support_check("Sleep hooks", &caps.sleep_hooks, false));

    let config_path = diagnostics
        .as_ref()
        .map_or(CONFIG_PATH.to_owned(), |d| d.config_path.clone());
    match ConfigStore::new(&config_path).load() {
        Ok(Some(_)) => checks.push(check(
            Level::Pass,
            "Config file",
            format!("{config_path} is valid"),
        )),
        Ok(None) => checks.push(check(
            Level::Info,
            "Config file",
            format!("{config_path} does not exist; defaults are in use"),
        )),
        Err(e) => checks.push(check(Level::Fail, "Config file", e.to_string())),
    }

    let plist = Path::new(PLIST_PATH).exists();
    let binary = Path::new(INSTALLED_BINARY).exists();
    checks.push(match (plist, binary) {
        (true, true) => check(
            Level::Pass,
            "LaunchDaemon",
            format!("{PLIST_PATH} installed"),
        ),
        (false, false) => check(
            Level::Info,
            "LaunchDaemon",
            format!("not installed; install wattbit with:\n    {INSTALL_COMMAND}"),
        ),
        _ => check(
            Level::Warn,
            "LaunchDaemon",
            format!("partially installed; repair it with:\n    {INSTALL_COMMAND}"),
        ),
    });

    if let Some(paths) = &speed {
        checks.push(speed_check(paths));
    }
    checks.extend(leftover_check());

    checks
}

/// Whether the speed meter runs; it is optional, so this is information.
fn speed_check(paths: &SpeedPaths) -> Check {
    match agent::live_agent(paths) {
        Some(meter) if meter.version.as_deref() == Some(env!("CARGO_PKG_VERSION")) => check(
            Level::Pass,
            "Speed meter",
            format!("on (pid {})", meter.pid),
        ),
        Some(meter) => check(
            Level::Warn,
            "Speed meter",
            format!(
                "on (pid {}), but running wattbit {}; restart it with `wattbit speed on`",
                meter.pid,
                meter.version.as_deref().unwrap_or("0.1")
            ),
        ),
        None => check(
            Level::Info,
            "Speed meter",
            "off; show it in the menu bar with `wattbit speed on`",
        ),
    }
}

/// Files berry, wattbit's previous name, left behind.
fn leftover_check() -> Option<Check> {
    let mut found: Vec<String> = legacy::system_leftovers()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Ok(home) = agent::home() {
        for relative in [
            legacy::SPEED_DIR.to_owned(),
            format!("Library/LaunchAgents/{}.plist", legacy::SPEED_LABEL),
        ] {
            let path = home.join(relative);
            if path.exists() {
                found.push(path.display().to_string());
            }
        }
    }
    (!found.is_empty()).then(|| {
        check(
            Level::Warn,
            "Old berry files",
            format!(
                "berry (wattbit's old name) left {}. Running the installer moves your \
                 settings and history over and removes them:\n    {INSTALL_COMMAND}",
                found.join(", ")
            ),
        )
    })
}

/// Reports other charge limiters, merging this process's scan with the
/// root daemon's (which can read every process's arguments).
fn conflict_checks(checks: &mut Vec<Check>, daemon: Option<&DaemonDiagnostics>) {
    let mut running = conflicts::running();
    for c in daemon.map_or(&[][..], |d| &d.conflicts) {
        if !running.iter().any(|r| r.pid == c.pid) {
            running.push(c.clone());
        }
    }
    let installed = conflicts::installed(&running);
    if running.is_empty() && installed.is_empty() {
        checks.push(check(Level::Pass, "Other charge limiters", "none found"));
        return;
    }
    let mut tools: Vec<&str> = Vec::new();
    for c in &running {
        if !tools.contains(&c.tool.as_str()) {
            tools.push(&c.tool);
        }
    }
    for tool in tools {
        let group: Vec<&Conflict> = running.iter().filter(|c| c.tool == tool).collect();
        let pids: Vec<String> = group
            .iter()
            .filter_map(|c| c.pid.map(|p| p.to_string()))
            .collect();
        checks.push(check(
            Level::Fail,
            format!("Conflict: {tool}"),
            format!(
                "running (pid {}: {}); wattbit pauses while it runs. Remove it with:\n    {}",
                pids.join(", "),
                group[0].detail,
                group[0].remedy
            ),
        ));
    }
    for c in installed {
        checks.push(check(
            Level::Warn,
            format!("Conflict: {}", c.tool),
            format!(
                "installed but not running ({}). Remove it with:\n    {}",
                c.detail, c.remedy
            ),
        ));
    }
}

fn key_checks(checks: &mut Vec<Check>, local: &Capabilities, daemon: Option<&DaemonDiagnostics>) {
    for probe in &local.keys {
        let verified = daemon
            .and_then(|d| d.capabilities.keys.iter().find(|k| k.key == probe.key))
            .and_then(|k| k.writable);
        let name = format!("SMC key {}", probe.key);
        let item = if !probe.present {
            check(Level::Info, name, format!("absent ({})", probe.purpose))
        } else if !probe.readable {
            check(
                Level::Warn,
                name,
                format!(
                    "{}: {}",
                    probe.purpose,
                    probe.note.as_deref().unwrap_or("not readable")
                ),
            )
        } else {
            let write = match verified {
                Some(true) => "writable (verified by daemon)",
                Some(false) => "not writable",
                None => "write access not tested",
            };
            check(
                if verified == Some(false) {
                    Level::Warn
                } else {
                    Level::Pass
                },
                name,
                format!(
                    "{}: readable, type {:?}, value [{}], {write}",
                    probe.purpose,
                    probe.data_type.as_deref().unwrap_or("?"),
                    probe.value.as_deref().unwrap_or("")
                ),
            )
        };
        checks.push(item);
    }
}

pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let _ = writeln!(out, "{} {}: {}", c.level.symbol(), c.name, c.detail);
    }
    let failures = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warnings = checks.iter().filter(|c| c.level == Level::Warn).count();
    let _ = writeln!(out, "\n{failures} failure(s), {warnings} warning(s)");
    out
}

pub fn has_failures(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.level == Level::Fail)
}
