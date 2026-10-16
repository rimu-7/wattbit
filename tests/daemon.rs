//! End-to-end test of the real binary: starts the daemon in read-only mode
//! on a private socket and config file, then drives it with the CLI.
//! Read-only mode never writes to the SMC and holds no power assertions,
//! so this test cannot change the machine's charging state.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::thread;
use std::time::{Duration, Instant};

const BERRY: &str = env!("CARGO_BIN_EXE_berry");

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn start(name: &str) -> Self {
        // Unix socket paths are limited to 104 bytes, so keep this short.
        let dir = PathBuf::from(format!("/tmp/berry-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let child = Command::new(BERRY)
            .args(["daemon", "run", "--read-only", "--socket"])
            .arg(dir.join("s"))
            .arg("--config")
            .arg(dir.join("berry.json"))
            .stderr(fs::File::create(dir.join("log")).unwrap())
            .spawn()
            .unwrap();
        let daemon = Daemon { child, dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !daemon.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "daemon did not start: {}",
                daemon.log()
            );
            thread::sleep(Duration::from_millis(50));
        }
        daemon
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("s")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("berry.json")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }

    fn berry(&self, args: &[&str]) -> Output {
        Command::new(BERRY)
            .args(args)
            .arg("--socket")
            .arg(self.socket())
            .output()
            .unwrap()
    }

    fn terminate(&mut self) -> std::process::ExitStatus {
        // SAFETY: signalling our own child process.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        self.child.wait().unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn status_settings_and_shutdown() {
    let mut daemon = Daemon::start("main");

    let status = daemon.berry(&["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let text = stdout(&status);
    for heading in [
        "Charging status:",
        "Battery status:",
        "Battery configuration:",
        "Hardware compatibility:",
        "Calibration status:",
    ] {
        assert!(text.contains(heading), "missing {heading} in:\n{text}");
    }
    assert!(text.contains("Upper limit: disabled"));

    let json = daemon.berry(&["status", "--json"]);
    assert!(json.status.success(), "{}", stderr(&json));
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["config"]["charge_limit"], serde_json::Value::Null);
    assert_eq!(report["capabilities"]["writes_verified"], false);
    assert_eq!(report["capabilities"]["sleep_hooks"]["status"], "supported");

    // Settings persist atomically to the daemon's config file.
    let set = daemon.berry(&["set", "prevent-system-sleep", "on"]);
    assert!(set.status.success(), "{}", stderr(&set));
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(daemon.config()).unwrap()).unwrap();
    assert_eq!(saved["prevent_system_sleep_when_charging"], true);
    let text = stdout(&daemon.berry(&["status"]));
    assert!(text.contains("Prevent system-sleep when charging: ✔"));

    // Disabling non-root access tightens the socket mode.
    let set = daemon.berry(&["set", "allow-non-root", "off"]);
    assert!(set.status.success(), "{}", stderr(&set));
    let mode = fs::metadata(daemon.socket()).map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o777
    });
    assert_eq!(mode.unwrap(), 0o600);

    // Doctor talks to the daemon and reports its uid.
    let doctor = daemon.berry(&["doctor"]);
    assert!(
        stdout(&doctor).contains("Daemon socket: berryd"),
        "{}",
        stdout(&doctor)
    );

    // SIGTERM shuts down cleanly and removes the socket.
    let exit = daemon.terminate();
    assert!(exit.success(), "{}", daemon.log());
    assert!(!daemon.socket().exists());
    assert!(daemon.log().contains("SIGTERM received"));
}

#[test]
fn unsupported_or_failed_writes_are_reported_not_faked() {
    let daemon = Daemon::start("limit");
    let output = daemon.berry(&["limit", "80"]);
    let code = output.status.code();
    let caps: serde_json::Value =
        serde_json::from_slice(&daemon.berry(&["status", "--json"]).stdout).unwrap();
    let native = caps["capabilities"]["charge_inhibit"]["status"] != "unsupported"
        || caps["capabilities"]["firmware_limit"]["status"] != "unsupported";
    if native {
        // The limit is saved, but read-only mode refuses to apply it, and
        // that refusal must surface as an error.
        assert_eq!(code, Some(1), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("read-only mode"),
            "{}",
            stderr(&output)
        );
    } else {
        // No charge-inhibit control: the daemon says so (exit status 3).
        assert_eq!(code, Some(3), "{}", stderr(&output));
        assert!(stderr(&output).contains("no usable charge-inhibit control"));
        assert!(
            !daemon.config().exists(),
            "nothing may be saved for an unsupported request"
        );
    }
}

#[test]
fn bad_input_is_rejected_by_the_cli() {
    let daemon = Daemon::start("input");
    let output = daemon.berry(&["limit", "120"]);
    assert_eq!(output.status.code(), Some(2));
    let output = daemon.berry(&["limit", "80", "--lower", "90"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("must be below the upper limit"));
}

#[test]
fn client_reports_missing_daemon() {
    let output = Command::new(BERRY)
        .args(["status", "--socket", "/tmp/berry-it-does-not-exist.sock"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("daemon is not running"));
}

#[test]
fn refuses_to_replace_a_regular_file() {
    let dir = PathBuf::from(format!("/tmp/berry-it-{}-file", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("s");
    fs::write(&socket, "precious").unwrap();
    let output = Command::new(BERRY)
        .args(["daemon", "run", "--read-only", "--socket"])
        .arg(&socket)
        .arg("--config")
        .arg(dir.join("c.json"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("is not a socket"));
    assert_eq!(fs::read_to_string(&socket).unwrap(), "precious");
    let _ = fs::remove_dir_all(Path::new(&dir));
}
