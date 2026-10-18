//! End-to-end test of the real binary's speed meter: drives `berry speed`
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

const BERRY: &str = env!("CARGO_BIN_EXE_berry");

struct Meter {
    dir: PathBuf,
}

impl Meter {
    fn new(name: &str) -> Self {
        // Unix socket paths are limited to 104 bytes, so keep this short.
        let dir = PathBuf::from(format!("/tmp/berry-speed-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    /// The binary with the sandbox environment applied: the data directory
    /// is private, `HOME` points inside it (so a stray LaunchAgent write
    /// cannot escape), and the menu bar stays disabled even in a GUI
    /// session.
    fn command(&self) -> Command {
        let mut command = Command::new(BERRY);
        command
            .env("BERRY_SPEED_DIR", &self.dir)
            .env("HOME", &self.dir)
            .env("BERRY_SPEED_NO_GUI", "1");
        command
    }

    fn berry(&self, args: &[&str]) -> Output {
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
}

impl Drop for Meter {
    fn drop(&mut self) {
        let _ = self.berry(&["speed", "off"]);
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

    let status = meter.berry(&["speed", "status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let text = stdout(&status);
    assert!(text.contains("Internet speed meter: off"), "{text}");
    assert!(text.contains("nothing recorded yet"), "{text}");

    let report = json(&meter.berry(&["speed", "status", "--json"]));
    assert_eq!(report["running"], false);
    assert_eq!(report["live"], serde_json::Value::Null);
    assert_eq!(report["usage"], serde_json::Value::Null);
    assert_eq!(report["warning"], serde_json::Value::Null);
}

#[test]
fn once_samples_and_saves_the_day() {
    let meter = Meter::new("once");

    let sample = meter.berry(&["speed", "agent", "--once", "--interval", "0.2"]);
    assert!(sample.status.success(), "{}", stderr(&sample));
    let live = json(&sample);
    assert!(live["download_bps"].is_number());
    assert!(live["upload_bps"].is_number());
    assert_eq!(live["interval_seconds"], 0.2);
    assert!(live["pid"].as_u64().unwrap() > 0);

    let store = json(&meter.berry(&["speed", "status", "--json"]));
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

    let on = meter.berry(&["speed", "on", "--no-launchd"]);
    assert!(on.status.success(), "{}", stderr(&on));
    assert!(stdout(&on).contains("Speed meter on"), "{}", stdout(&on));
    assert!(meter.pid_file().exists());
    assert!(meter.socket().exists());

    // A second `on` notices the running meter instead of starting another.
    let again = meter.berry(&["speed", "on", "--no-launchd"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("already on"), "{}", stdout(&again));

    let status = meter.berry(&["speed", "status"]);
    let text = stdout(&status);
    assert!(text.contains("Internet speed meter: on"), "{text}");
    assert!(text.contains("Live speed:"), "{text}");

    let report = json(&meter.berry(&["speed", "status", "--json"]));
    assert_eq!(report["running"], true);
    assert_eq!(report["live"]["interval_seconds"], 1.0);

    // Let at least one sampling tick land so the store has a day entry.
    std::thread::sleep(Duration::from_millis(1200));

    let off = meter.berry(&["speed", "off"]);
    assert!(off.status.success(), "{}", stderr(&off));
    assert!(stdout(&off).contains("Speed meter off"), "{}", stdout(&off));
    assert!(!meter.pid_file().exists());
    assert!(!meter.socket().exists());
    assert!(meter.store().exists(), "usage history must survive `off`");

    let store: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(meter.store()).unwrap()).unwrap();
    assert_eq!(store["version"], 1);
    assert_eq!(store["days"].as_array().unwrap().len(), 1);

    let status = meter.berry(&["speed", "status"]);
    assert!(stdout(&status).contains("Internet speed meter: off"));

    // `off` twice is graceful.
    let off = meter.berry(&["speed", "off"]);
    assert!(off.status.success(), "{}", stderr(&off));
    assert!(stdout(&off).contains("already off"), "{}", stdout(&off));
}
