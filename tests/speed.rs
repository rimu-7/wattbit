//! End-to-end test of the real binary's speed meter: drives `wattbit speed`
//! against a private data directory, with the menu bar disabled and no
//! launchd interaction (`--no-launchd`), so the test never installs a login
//! item and never touches `~/Library`.
//!
//! The agent itself samples the machine's real interface counters; these
//! tests only assert the wiring (commands, files, socket, clean shutdown),
//! not the byte counts, which depend on what else the machine is doing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

const WATTBIT: &str = env!("CARGO_BIN_EXE_wattbit");

struct Meter {
    dir: PathBuf,
}

impl Meter {
    fn new(name: &str) -> Self {
        // Unix socket paths are limited to 104 bytes, so keep this short.
        let dir = PathBuf::from(format!(
            "/tmp/wattbit-speed-it-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    /// The binary with the sandbox environment applied: the data directory
    /// is private, `HOME` points inside it (so a stray LaunchAgent write
    /// cannot escape), the menu bar stays disabled even in a GUI session,
    /// and the agent never asks GitHub for releases.
    fn command(&self) -> Command {
        let mut command = Command::new(WATTBIT);
        command
            .env("WATTBIT_SPEED_DIR", &self.dir)
            .env("HOME", &self.dir)
            .env("WATTBIT_SPEED_NO_GUI", "1")
            .env("WATTBIT_NO_UPDATE_CHECK", "1");
        command
    }

    fn wattbit(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn store(&self) -> PathBuf {
        self.dir.join("speed.json")
    }

    fn pid_file(&self) -> PathBuf {
        self.dir.join("speed.pid")
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("speed.sock")
    }

    fn pid(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(self.pid_file()).unwrap()).unwrap()
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        let _ = self.wattbit(&["speed", "off"]);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", stdout(output)))
}

#[test]
fn status_is_readable_on_a_fresh_install() {
    let meter = Meter::new("fresh");

    let status = meter.wattbit(&["speed", "status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let text = stdout(&status);
    assert!(text.contains("Internet speed meter: off"), "{text}");
    assert!(text.contains("nothing recorded yet"), "{text}");

    let report = json(&meter.wattbit(&["speed", "status", "--json"]));
    assert_eq!(report["running"], false);
    assert_eq!(report["live"], serde_json::Value::Null);
    assert_eq!(report["usage"], serde_json::Value::Null);
    assert_eq!(report["warning"], serde_json::Value::Null);
}

#[test]
fn once_samples_and_saves_the_day() {
    let meter = Meter::new("once");

    let sample = meter.wattbit(&["speed", "agent", "--once", "--interval", "0.2"]);
    assert!(sample.status.success(), "{}", stderr(&sample));
    let live = json(&sample);
    assert!(live["download_bps"].is_number());
    assert!(live["upload_bps"].is_number());
    assert_eq!(live["interval_seconds"], 0.2);
    assert!(live["pid"].as_u64().unwrap() > 0);

    let store = json(&meter.wattbit(&["speed", "status", "--json"]));
    let days = store["usage"]["days"].as_array().unwrap();
    assert_eq!(days.len(), 1, "one day should have been recorded");
    assert_eq!(store["usage"]["recorded_days"], 1);

    // A single sample leaves no process behind.
    assert!(!meter.pid_file().exists());
    assert!(!meter.socket().exists());
}

#[test]
fn on_status_and_off_round_trip() {
    let meter = Meter::new("roundtrip");

    let on = meter.wattbit(&["speed", "on", "--no-launchd"]);
    assert!(on.status.success(), "{}", stderr(&on));
    assert!(stdout(&on).contains("Speed meter on"), "{}", stdout(&on));
    assert!(meter.pid_file().exists());
    assert!(meter.socket().exists());

    // A second `on` notices the running meter instead of starting another.
    let again = meter.wattbit(&["speed", "on", "--no-launchd"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("already on"), "{}", stdout(&again));

    let status = meter.wattbit(&["speed", "status"]);
    let text = stdout(&status);
    assert!(text.contains("Internet speed meter: on"), "{text}");
    assert!(text.contains("Live speed:"), "{text}");

    let report = json(&meter.wattbit(&["speed", "status", "--json"]));
    assert_eq!(report["running"], true);
    assert_eq!(report["live"]["interval_seconds"], 1.0);

    // Let at least one sampling tick land so the store has a day entry.
    std::thread::sleep(Duration::from_millis(1200));

    let off = meter.wattbit(&["speed", "off"]);
    assert!(off.status.success(), "{}", stderr(&off));
    assert!(stdout(&off).contains("Speed meter off"), "{}", stdout(&off));
    assert!(!meter.pid_file().exists());
    assert!(!meter.socket().exists());
    assert!(meter.store().exists(), "usage history must survive `off`");

    let store: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(meter.store()).unwrap()).unwrap();
    assert_eq!(store["version"], 1);
    assert_eq!(store["days"].as_array().unwrap().len(), 1);

    let status = meter.wattbit(&["speed", "status"]);
    assert!(stdout(&status).contains("Internet speed meter: off"));

    // `off` twice is graceful.
    let off = meter.wattbit(&["speed", "off"]);
    assert!(off.status.success(), "{}", stderr(&off));
    assert!(stdout(&off).contains("already off"), "{}", stdout(&off));
}

#[test]
fn a_cached_release_is_offered_with_its_command() {
    let meter = Meter::new("update");
    let command =
        "curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash";

    // Nothing newer known: no notice anywhere.
    let report = json(&meter.wattbit(&["speed", "status", "--json"]));
    assert_eq!(report["update"], serde_json::Value::Null);

    fs::write(
        meter.dir.join("update.json"),
        r#"{"checked_at": 1700000000, "latest": "999.0.0"}"#,
    )
    .unwrap();
    let report = json(&meter.wattbit(&["speed", "status", "--json"]));
    assert_eq!(report["update"]["latest"], "999.0.0");
    assert_eq!(report["update"]["current"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["update"]["command"], command);
    let text = stdout(&meter.wattbit(&["speed", "status"]));
    assert!(
        text.contains("Update available: wattbit 999.0.0") && text.contains(command),
        "{text}"
    );

    // The release this build already is (or an older one) is not news.
    fs::write(
        meter.dir.join("update.json"),
        format!(
            r#"{{"checked_at": 1700000000, "latest": "{}"}}"#,
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    let report = json(&meter.wattbit(&["speed", "status", "--json"]));
    assert_eq!(report["update"], serde_json::Value::Null);
}

#[test]
fn an_outdated_meter_is_replaced_by_on() {
    let meter = Meter::new("upgrade");

    let on = meter.wattbit(&["speed", "on", "--no-launchd"]);
    assert!(on.status.success(), "{}", stderr(&on));
    let first = meter.pid();
    assert_eq!(first["version"], env!("CARGO_PKG_VERSION"));

    // Pretend the running agent predates this build, as it does right
    // after an update replaced the binary underneath it.
    fs::write(
        meter.pid_file(),
        format!(
            r#"{{"pid":{},"started_at":{}}}"#,
            first["pid"], first["started_at"]
        ),
    )
    .unwrap();

    let again = meter.wattbit(&["speed", "on", "--no-launchd"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(
        stdout(&again).contains("Speed meter restarted on wattbit"),
        "{}",
        stdout(&again)
    );
    let second = meter.pid();
    assert_ne!(first["pid"], second["pid"], "a new agent should be running");
    assert_eq!(second["version"], env!("CARGO_PKG_VERSION"));
    let old_pid = first["pid"].as_i64().unwrap() as libc::pid_t;
    // SAFETY: signal 0 only probes whether the process exists.
    let probe = unsafe { libc::kill(old_pid, 0) };
    assert_ne!(probe, 0, "the old agent must be gone");
}

#[test]
fn berry_history_moves_over_on_the_first_on() {
    let meter = Meter::new("berry");
    let old_dir = meter.dir.join("Library/Application Support/berry");
    fs::create_dir_all(&old_dir).unwrap();
    let today = stdout(&Command::new("/bin/date").arg("+%Y-%m-%d").output().unwrap());
    let today = today.trim();
    fs::write(
        old_dir.join("speed.json"),
        format!(
            r#"{{"version":1,"days":[{{"date":"{today}","download_bytes":123456,"upload_bytes":789}}]}}"#
        ),
    )
    .unwrap();
    fs::write(old_dir.join("speed.log"), b"berry log").unwrap();

    let on = meter.wattbit(&["speed", "on", "--no-launchd"]);
    assert!(on.status.success(), "{}", stderr(&on));
    let text = stdout(&on);
    assert!(text.contains("Moved over from berry"), "{text}");
    assert!(
        !old_dir.exists(),
        "berry's directory should be gone: {text}"
    );

    let report = json(&meter.wattbit(&["speed", "status", "--json"]));
    let downloaded = report["usage"]["today"]["download_bytes"].as_u64().unwrap();
    assert!(
        downloaded >= 123_456,
        "berry's bytes must be kept: {report}"
    );
}
