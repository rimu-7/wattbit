//! The sampling agent behind `berry speed on`.
//!
//! The agent runs inside the user's login session (as a LaunchAgent, or
//! spawned directly when launchd is unavailable). Its main thread owns the
//! menu bar item, while a small Tokio runtime on a worker thread samples
//! the interface counters once per interval, accumulates usage into the
//! store, answers live-status queries on a Unix socket, and stops when the
//! process is signalled or the menu's *Turn off* item is clicked.
//!
//! Usage data is flushed to disk every [`SAVE_INTERVAL`] and on exit; the
//! menu bar updates every tick from memory.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener as SyncUnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::{UnixListener as AsyncUnixListener, UnixStream as AsyncUnixStream};
use tokio::time::MissedTickBehavior;

use super::date::{self, LocalDate};
use super::sampler::{self, Counters};
use super::store::{UsageStore, RETENTION_DAYS};
use super::{menubar, stop_requested, Display};
use crate::service;

/// Writes a timestamped line to stderr (captured by launchd into the log).
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!(
            "{} berry-speed[{}]: {}",
            crate::daemon::timestamp(),
            std::process::id(),
            format_args!($($arg)*)
        )
    };
}

/// LaunchAgent label.
pub const LABEL: &str = "com.berry.speed";

/// Sampling interval used when none is given.
pub const DEFAULT_INTERVAL_SECONDS: f64 = 1.0;

/// Shortest accepted sampling interval.
pub const MIN_INTERVAL_SECONDS: f64 = 0.2;

/// Longest accepted sampling interval.
pub const MAX_INTERVAL_SECONDS: f64 = 60.0;

/// How often accumulated usage is flushed to disk.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long the UI loop waits for a new sample before pumping the run loop.
const UI_POLL: Duration = Duration::from_millis(250);

/// How long `on` waits for the agent to come up.
const START_TIMEOUT: Duration = Duration::from_secs(15);

/// How long `off` waits for a signalled agent to exit before force-killing.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a starting agent waits for a predecessor to exit.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(3);

/// Upper bound on one live-status conversation.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(2);

/// Sample gaps longer than this still add traffic to the totals, but are
/// not turned into a rate (they would read as a spike).
const RATE_WINDOW_MAX_SECONDS: f64 = 30.0;

/// The agent truncates its own log when it grows past this on startup.
const LOG_MAX_BYTES: u64 = 512 * 1024;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// Files and directories the speed meter uses.
#[derive(Debug, Clone)]
pub struct SpeedPaths {
    /// Data directory (created with mode 0700).
    pub dir: PathBuf,
    /// Usage history.
    pub store: PathBuf,
    /// The running agent's pid file.
    pub pid: PathBuf,
    /// Live-status socket.
    pub socket: PathBuf,
    /// Agent log.
    pub log: PathBuf,
}

impl SpeedPaths {
    /// Resolves the data directory: `$BERRY_SPEED_DIR` when set (used by
    /// tests and unusual setups), otherwise `~/Library/Application Support/berry`.
    pub fn resolve() -> anyhow::Result<Self> {
        let dir = match std::env::var_os("BERRY_SPEED_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => home()?.join("Library/Application Support/berry"),
        };
        Ok(Self {
            store: dir.join("speed.json"),
            pid: dir.join("speed.pid"),
            socket: dir.join("speed.sock"),
            log: dir.join("speed.log"),
            dir,
        })
    }

    /// The LaunchAgent property list, under the user's `Library`.
    pub fn plist(&self) -> anyhow::Result<PathBuf> {
        Ok(home()?
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }
}

fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .context("HOME is not set")
}

/// Ensures the data directory exists and is private.
pub fn ensure_dir(paths: &SpeedPaths) -> anyhow::Result<()> {
    fs::create_dir_all(&paths.dir).with_context(|| format!("creating {}", paths.dir.display()))?;
    fs::set_permissions(&paths.dir, fs::Permissions::from_mode(DIR_MODE))
        .with_context(|| format!("securing {}", paths.dir.display()))?;
    Ok(())
}

/// The agent's pid file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidFile {
    pub pid: u32,
    pub started_at: i64,
}

impl PidFile {
    fn read(path: &Path) -> Option<Self> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    }

    /// Creates the pid file. `Ok(false)` means it already existed.
    fn create(path: &Path, contents: Self) -> io::Result<bool> {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(path)
        {
            Ok(mut file) => {
                file.write_all(contents.to_json().as_bytes())?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The file's contents as JSON.
    fn to_json(self) -> String {
        format!(
            "{{\"pid\":{},\"started_at\":{}}}\n",
            self.pid, self.started_at
        )
    }
}

/// Live figures served on the status socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveState {
    pub pid: u32,
    pub started_at: i64,
    pub sample_at: i64,
    pub interval_seconds: f64,
    pub download_bps: f64,
    pub upload_bps: f64,
    pub today_download_bytes: u64,
    pub today_upload_bytes: u64,
    pub window_download_bytes: u64,
    pub window_upload_bytes: u64,
}

/// Options for running the agent in the foreground.
#[derive(Debug, Clone, Copy)]
pub struct AgentOptions {
    pub interval: f64,
    /// Skip the menu bar even in a GUI session (used by tests).
    pub headless: bool,
}

/// `true` when the process exists (even if it is not ours to signal).
pub fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes for existence and permission.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn executable_of(pid: u32) -> Option<PathBuf> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buffer` is writable and `buffer.len()` names its size.
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::pid_t,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if len <= 0 {
        return None;
    }
    buffer.truncate(len as usize);
    String::from_utf8(buffer).ok().map(PathBuf::from)
}

fn current_binary() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
}

/// The agent owning the pid file, if it is alive and really berry.
pub fn live_agent(paths: &SpeedPaths) -> Option<PidFile> {
    let pid_file = PidFile::read(&paths.pid)?;
    if !process_alive(pid_file.pid) {
        return None;
    }
    // A recycled pid could belong to anything; only trust it when the
    // executable still matches the one running this code.
    if let (Some(executable), Some(ours)) = (executable_of(pid_file.pid), current_binary()) {
        if executable != ours {
            return None;
        }
    }
    Some(pid_file)
}

/// Asks a process to stop, escalating to SIGKILL after `timeout`.
fn terminate(pid: u32, timeout: Duration) -> bool {
    for (signal, wait) in [
        (libc::SIGTERM, timeout),
        (libc::SIGKILL, Duration::from_secs(2)),
    ] {
        if !process_alive(pid) {
            return true;
        }
        // SAFETY: the pid comes from berry's own pid file, and we verified
        // its executable path before calling.
        unsafe { libc::kill(pid as libc::pid_t, signal) };
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if !process_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    !process_alive(pid)
}

/// `berry speed on`: starts the meter and prints what happened.
pub fn on(paths: &SpeedPaths, no_launchd: bool) -> anyhow::Result<String> {
    ensure_user("on")?;
    ensure_dir(paths)?;
    if let Some(agent) = live_agent(paths) {
        return Ok(format!(
            "The speed meter is already on (pid {}).",
            agent.pid
        ));
    }
    remove_leftovers(paths);

    let mut via_login_item = false;
    if no_launchd {
        // Explicitly direct: drop any login item left from an earlier run.
        if launchd_loaded()? {
            bootout()?;
        }
        remove_plist(paths)?;
    } else {
        match start_via_launchd(paths) {
            Ok(()) => via_login_item = true,
            Err(e) => {
                log!("launchd could not start the agent ({e:#}); starting it directly");
                let _ = bootout();
                remove_plist(paths)?;
            }
        }
    }
    if !via_login_item {
        start_direct(paths)?;
    }
    let agent = wait_for_agent(paths, START_TIMEOUT)?;

    let mut message = format!(
        "Speed meter on (pid {}). The menu bar now shows the current ↓/↑ rates.",
        agent.pid
    );
    if via_login_item {
        message.push_str(" It starts again automatically at login.");
    }
    message.push_str(&format!(
        " Turn it off with `berry speed off`.\n\
         Usage is recorded in {} and kept for {RETENTION_DAYS} days.",
        paths.store.display()
    ));
    Ok(message)
}

/// `berry speed off`: stops the meter, keeping the usage history.
pub fn off(paths: &SpeedPaths) -> anyhow::Result<String> {
    ensure_user("off")?;
    let mut done = Vec::new();
    if launchd_loaded()? {
        bootout()?;
        done.push("stopped the login item".to_owned());
    }
    if remove_plist(paths)? {
        done.push("removed the login item file".to_owned());
    }
    if let Some(agent) = live_agent(paths) {
        if terminate(agent.pid, STOP_TIMEOUT) {
            done.push(format!("stopped the meter (pid {})", agent.pid));
        } else {
            bail!(
                "the speed meter (pid {}) did not stop; try `kill -9 {}`",
                agent.pid,
                agent.pid
            );
        }
    }
    remove_leftovers(paths);
    // Enforce the retention window even if the meter is not started again
    // for a long time, so the promise "older than 31 days is deleted" holds.
    let mut store = load_store(paths);
    if store.prune(LocalDate::today()) > 0 {
        match store.save(&paths.store) {
            Ok(()) => done.push("deleted usage records older than the window".to_owned()),
            Err(e) => log!("cannot save the pruned usage history: {e}"),
        }
    }
    let summary = if done.is_empty() {
        "The speed meter is already off.".to_owned()
    } else {
        format!("Speed meter off: {}.", done.join(", "))
    };
    Ok(format!(
        "{summary}\nUsage history is kept in {} and is deleted automatically after {RETENTION_DAYS} days.",
        paths.store.display()
    ))
}

fn ensure_user(command: &str) -> anyhow::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        bail!(
            "`berry speed {command}` runs in your own login session and must not run as root; \
             re-run it without sudo"
        );
    }
    Ok(())
}

fn remove_leftovers(paths: &SpeedPaths) {
    for file in [&paths.pid, &paths.socket] {
        match fs::remove_file(file) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => log!("cannot remove {}: {e}", file.display()),
        }
    }
}

fn truncate_log(paths: &SpeedPaths) {
    let large = fs::metadata(&paths.log).is_ok_and(|meta| meta.len() > LOG_MAX_BYTES);
    if large {
        let _ = fs::write(&paths.log, b"");
    }
}

fn domain() -> String {
    // SAFETY: geteuid has no preconditions.
    format!("gui/{}", unsafe { libc::geteuid() })
}

fn launchd_target() -> String {
    format!("{}/{}", domain(), LABEL)
}

fn launchd_loaded() -> anyhow::Result<bool> {
    Ok(service::launchctl(&["print", &launchd_target()])?
        .status
        .success())
}

fn bootout() -> anyhow::Result<()> {
    service::launchctl_checked(&["bootout", &launchd_target()])
}

/// Renders the LaunchAgent property list for `binary`.
///
/// `RunAtLoad` starts the meter at login; `KeepAlive` restarts it if it
/// crashes (an unsuccessful exit) but not after a clean stop — the menu's
/// *Turn off* item exits with status 0, and the job must stay down then.
pub fn plist(binary: &Path, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{binary}</string>
		<string>speed</string>
		<string>agent</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = service::xml_escape(LABEL),
        binary = service::xml_escape(&binary.to_string_lossy()),
        log = service::xml_escape(&log.to_string_lossy()),
    )
}

fn write_file(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
    let contents = contents.to_vec();
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let write = (|| -> io::Result<()> {
        let _ = fs::remove_file(&tmp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(&contents)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write.with_context(|| format!("writing {}", path.display()))
}

fn remove_plist(paths: &SpeedPaths) -> anyhow::Result<bool> {
    let Ok(plist) = paths.plist() else {
        return Ok(false);
    };
    let ours = fs::read_to_string(&plist)
        .map(|text| text.contains(&format!("<string>{LABEL}</string>")))
        .unwrap_or(false);
    if !ours {
        return Ok(false);
    }
    match fs::remove_file(&plist) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", plist.display())),
    }
}

/// Loads the store, quarantining a corrupt file so the agent can start.
fn load_store(paths: &SpeedPaths) -> UsageStore {
    match UsageStore::load(&paths.store) {
        Ok(Some(store)) => store,
        Ok(None) => UsageStore::default(),
        Err(e) => {
            log!("{e}");
            match UsageStore::quarantine(&paths.store) {
                Ok(Some(moved)) => log!("moved the unreadable file to {}", moved.display()),
                Ok(None) => {}
                Err(move_error) => log!("cannot move the unreadable file: {move_error}"),
            }
            UsageStore::default()
        }
    }
}

fn start_via_launchd(paths: &SpeedPaths) -> anyhow::Result<()> {
    let binary = current_binary().context("locating the running executable")?;
    let plist_path = paths.plist()?;
    if launchd_loaded()? {
        bootout()?;
    }
    write_file(&plist_path, plist(&binary, &paths.log).as_bytes(), 0o644)?;
    let target = launchd_target();
    service::launchctl_checked(&["enable", &target])?;
    service::launchctl_checked(&["bootstrap", &domain(), &plist_path.to_string_lossy()])?;
    wait_for_agent(paths, START_TIMEOUT)
        .context("launchd accepted the job but the agent did not start")?;
    Ok(())
}

fn start_direct(paths: &SpeedPaths) -> anyhow::Result<()> {
    let binary = current_binary().context("locating the running executable")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(&paths.log)
        .with_context(|| format!("opening {}", paths.log.display()))?;
    let mut command = Command::new(&binary);
    command
        .args(["speed", "agent"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    if let Some(dir) = std::env::var_os("BERRY_SPEED_DIR") {
        command.env("BERRY_SPEED_DIR", dir);
    }
    if let Some(no_gui) = std::env::var_os("BERRY_SPEED_NO_GUI") {
        command.env("BERRY_SPEED_NO_GUI", no_gui);
    }
    // Detach from the controlling terminal so closing the terminal that
    // ran `berry speed on` does not stop the meter.
    // SAFETY: setsid only affects the child between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().context("starting the speed agent")?;
    wait_for_agent(paths, START_TIMEOUT).with_context(|| {
        format!(
            "the agent (pid {}) exited before it was ready; see {}",
            child.id(),
            paths.log.display()
        )
    })?;
    Ok(())
}

fn wait_for_agent(paths: &SpeedPaths, timeout: Duration) -> anyhow::Result<PidFile> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(agent) = live_agent(paths) {
            return Ok(agent);
        }
        if Instant::now() >= deadline {
            bail!(
                "the speed agent did not start within {} s",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Queries the running agent for live figures over its socket.
pub fn live_state(paths: &SpeedPaths) -> Option<LiveState> {
    let stream = UnixStream::connect(&paths.socket).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(1500)))
        .ok()?;
    (&stream).write_all(b"status\n").ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

/// A one-shot sample for `berry speed agent --once` and the tests. Does not
/// touch the pid file, the socket or the menu bar.
pub fn sample_once(paths: &SpeedPaths, interval: f64) -> anyhow::Result<LiveState> {
    ensure_dir(paths)?;
    let mut store = load_store(paths);
    let interval = interval.max(MIN_INTERVAL_SECONDS);
    let baseline = sampler::totals().context("reading interface counters")?;
    std::thread::sleep(Duration::from_secs_f64(interval));
    let current = sampler::totals().context("reading interface counters")?;
    let delta = current.delta_since(baseline);
    let today = LocalDate::today();
    store.prune(today);
    store.add(today, delta.received, delta.sent);
    store.save(&paths.store).context("saving usage")?;
    Ok(LiveState {
        pid: std::process::id(),
        started_at: date::now_unix(),
        sample_at: date::now_unix(),
        interval_seconds: interval,
        download_bps: delta.received as f64 / interval,
        upload_bps: delta.sent as f64 / interval,
        today_download_bytes: store.day(today).map_or(0, |d| d.download_bytes),
        today_upload_bytes: store.day(today).map_or(0, |d| d.upload_bytes),
        window_download_bytes: store.window(today).iter().map(|d| d.download_bytes).sum(),
        window_upload_bytes: store.window(today).iter().map(|d| d.upload_bytes).sum(),
    })
}

/// Keeps exclusive-ish ownership of the pid file, removing it on clean exit.
struct PidGuard {
    path: PathBuf,
    pid: u32,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if PidFile::read(&self.path).is_some_and(|file| file.pid == self.pid) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Claims the pid file, taking over from a predecessor if one is running.
fn claim(paths: &SpeedPaths) -> anyhow::Result<PidGuard> {
    let me = PidFile {
        pid: std::process::id(),
        started_at: date::now_unix(),
    };
    let guard = PidGuard {
        path: paths.pid.clone(),
        pid: me.pid,
    };
    match PidFile::create(&paths.pid, me) {
        Ok(true) => Ok(guard),
        Ok(false) => {
            if let Some(existing) = live_agent(paths) {
                if existing.pid != me.pid {
                    log!(
                        "another speed agent (pid {}) is running; stopping it first",
                        existing.pid
                    );
                    terminate(existing.pid, TAKEOVER_TIMEOUT);
                }
            }
            let _ = fs::remove_file(&paths.pid);
            PidFile::create(&paths.pid, me).context("writing the pid file")?;
            Ok(guard)
        }
        Err(e) => Err(e).context("writing the pid file"),
    }
}

/// The shared state of the sampling thread and the status socket.
#[derive(Debug)]
struct AgentState {
    store: UsageStore,
    store_path: PathBuf,
    previous_totals: Option<Counters>,
    last_sample_at: Instant,
    rates: (f64, f64),
    last_save: Instant,
    dirty: bool,
    today: LocalDate,
    started_at: i64,
    interval: f64,
    save_errors_logged: bool,
}

impl AgentState {
    fn today_usage(&self) -> (u64, u64) {
        self.store
            .day(self.today)
            .map_or((0, 0), |day| (day.download_bytes, day.upload_bytes))
    }

    fn window_usage(&self) -> (u64, u64) {
        self.store
            .window(self.today)
            .iter()
            .fold((0u64, 0u64), |(down, up), day| {
                (
                    down.saturating_add(day.download_bytes),
                    up.saturating_add(day.upload_bytes),
                )
            })
    }

    fn display(&self) -> Display {
        let (today_down, today_up) = self.today_usage();
        let (window_down, window_up) = self.window_usage();
        Display {
            download_bps: self.rates.0,
            upload_bps: self.rates.1,
            today_download_bytes: today_down,
            today_upload_bytes: today_up,
            window_download_bytes: window_down,
            window_upload_bytes: window_up,
        }
    }

    fn live_state(&self) -> LiveState {
        let (today_down, today_up) = self.today_usage();
        let (window_down, window_up) = self.window_usage();
        LiveState {
            pid: std::process::id(),
            started_at: self.started_at,
            sample_at: date::now_unix(),
            interval_seconds: self.interval,
            download_bps: self.rates.0,
            upload_bps: self.rates.1,
            today_download_bytes: today_down,
            today_upload_bytes: today_up,
            window_download_bytes: window_down,
            window_upload_bytes: window_up,
        }
    }

    fn lock(state: &Arc<Mutex<Self>>) -> std::sync::MutexGuard<'_, Self> {
        state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Flushes the store to disk when it has changed.
    fn persist(&mut self) {
        if !self.dirty {
            return;
        }
        match self.store.save(&self.store_path) {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
                self.save_errors_logged = false;
            }
            Err(e) => {
                if !self.save_errors_logged {
                    log!("cannot save usage: {e}");
                    self.save_errors_logged = true;
                }
            }
        }
    }
}

/// Runs the agent until it is signalled. This is the process `berry speed
/// on` leaves behind.
pub fn run(paths: &SpeedPaths, options: AgentOptions) -> anyhow::Result<()> {
    ensure_dir(paths)?;
    truncate_log(paths);
    let _guard = claim(paths)?;

    let mut store = load_store(paths);
    let today = LocalDate::today();
    let pruned = store.prune(today);
    if pruned > 0 {
        log!("deleted {pruned} usage record(s) outside the {RETENTION_DAYS}-day window");
    }

    let headless = options.headless || std::env::var_os("BERRY_SPEED_NO_GUI").is_some();
    let ui = if headless {
        None
    } else {
        match menubar::Ui::create() {
            Ok(ui) => Some(ui),
            Err(reason) => {
                log!("menu bar unavailable: {reason}; recording usage only");
                None
            }
        }
    };
    log!(
        "sampling every {:.0} ms{}",
        options.interval * 1000.0,
        if ui.is_some() {
            "; menu bar item created"
        } else {
            "; headless"
        }
    );

    let listener = match bind_socket(paths) {
        Ok(listener) => Some(listener),
        Err(e) => {
            log!("live status socket unavailable: {e:#}");
            None
        }
    };

    let state = Arc::new(Mutex::new(AgentState {
        store,
        store_path: paths.store.clone(),
        previous_totals: None,
        last_sample_at: Instant::now(),
        rates: (0.0, 0.0),
        last_save: Instant::now(),
        dirty: false,
        today,
        started_at: date::now_unix(),
        interval: options.interval,
        save_errors_logged: false,
    }));
    let (updates_tx, updates_rx) = mpsc::channel();
    let worker_state = Arc::clone(&state);
    let worker = std::thread::Builder::new()
        .name("berry-speed-sampler".into())
        .spawn(move || -> anyhow::Result<()> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building the sampler runtime")?;
            runtime.block_on(async move {
                let listener = match listener {
                    Some(listener) => Some(
                        AsyncUnixListener::from_std(listener)
                            .context("registering the live status socket")?,
                    ),
                    None => None,
                };
                sample_loop(worker_state, listener, updates_tx, options.interval).await
            })
        })
        .context("starting the sampling thread")?;

    install_signal_handlers();
    ui_loop(ui, updates_rx);

    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            log!("sampler stopped: {e:#}");
            return Err(e);
        }
        Err(_) => bail!("the sampling thread panicked"),
    }
    remove_leftovers(paths);
    log!("speed meter stopped");
    Ok(())
}

/// Creates the live-status socket. The listener is bound synchronously, so
/// the caller does not need a Tokio runtime yet; [`run`] hands it to the
/// sampler thread, which wraps it with [`AsyncUnixListener::from_std`].
fn bind_socket(paths: &SpeedPaths) -> anyhow::Result<SyncUnixListener> {
    match fs::symlink_metadata(&paths.socket) {
        Ok(meta) if meta.file_type().is_socket() => {
            fs::remove_file(&paths.socket)
                .with_context(|| format!("removing stale {}", paths.socket.display()))?;
        }
        Ok(_) => bail!(
            "{} exists and is not a socket; refusing to replace it",
            paths.socket.display()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", paths.socket.display())),
    }
    let listener = SyncUnixListener::bind(&paths.socket)
        .with_context(|| format!("binding {}", paths.socket.display()))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("arming {}", paths.socket.display()))?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(FILE_MODE))
        .with_context(|| format!("securing {}", paths.socket.display()))?;
    Ok(listener)
}

extern "C" fn handle_signal(_signal: libc::c_int) {
    super::request_stop();
}

fn install_signal_handlers() {
    // SAFETY: the handler only stores into an atomic flag, which is
    // async-signal-safe; installing it only changes this process.
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as *const () as usize);
        libc::signal(libc::SIGINT, handle_signal as *const () as usize);
        libc::signal(libc::SIGHUP, handle_signal as *const () as usize);
    }
}

/// The main thread: pumps AppKit and paints new samples.
fn ui_loop(ui: Option<menubar::Ui>, updates: Receiver<Display>) {
    let mut ui = ui;
    let mut latest = Display::default();
    let mut unpainted = false;
    let mut placement_logged = false;
    loop {
        if stop_requested() {
            break;
        }
        match updates.recv_timeout(UI_POLL) {
            Ok(display) => {
                latest = display;
                unpainted = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(ui) = ui.as_mut() {
            if unpainted {
                ui.refresh(&latest);
                unpainted = false;
            }
            ui.pump(UI_POLL.as_secs_f64());
            if !placement_logged {
                log!("menu bar item: {}", ui.placement());
                placement_logged = true;
            }
        }
    }
}

async fn sample_loop(
    state: Arc<Mutex<AgentState>>,
    listener: Option<AsyncUnixListener>,
    updates: Sender<Display>,
    interval: f64,
) -> anyhow::Result<()> {
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(interval));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if stop_requested() {
                    break;
                }
                tick(&state, &updates);
            }
            accepted = accept(&listener) => {
                if let Some(stream) = accepted {
                    serve_state(&state, stream).await;
                }
            }
        }
    }
    AgentState::lock(&state).persist();
    Ok(())
}

async fn accept(listener: &Option<AsyncUnixListener>) -> Option<AsyncUnixStream> {
    match listener {
        Some(listener) => listener.accept().await.ok().map(|(stream, _)| stream),
        None => std::future::pending().await,
    }
}

fn tick(state: &Arc<Mutex<AgentState>>, updates: &Sender<Display>) {
    let mut state = AgentState::lock(state);
    let now = Instant::now();
    let elapsed = now.duration_since(state.last_sample_at).as_secs_f64();
    let totals = match sampler::totals() {
        Ok(totals) => totals,
        Err(e) => {
            state.rates = (0.0, 0.0);
            log!("cannot read interface counters: {e}");
            return;
        }
    };
    let baseline_only = state.previous_totals.is_none();
    let delta = match state.previous_totals {
        Some(previous) => totals.delta_since(previous),
        None => Counters::default(),
    };
    state.previous_totals = Some(totals);
    state.rates = if baseline_only || elapsed <= 0.0 || elapsed > RATE_WINDOW_MAX_SECONDS {
        (0.0, 0.0)
    } else {
        (delta.received as f64 / elapsed, delta.sent as f64 / elapsed)
    };
    state.last_sample_at = now;

    state.today = LocalDate::today();
    let today = state.today;
    state.store.add(today, delta.received, delta.sent);
    if state.store.prune(today) > 0 {
        log!("deleted usage records outside the {RETENTION_DAYS}-day window");
    }
    state.dirty = true;
    if state.last_save.elapsed() >= SAVE_INTERVAL {
        state.persist();
    }

    let display = state.display();
    drop(state);
    let _ = updates.send(display);
}

async fn serve_state(state: &Arc<Mutex<AgentState>>, stream: AsyncUnixStream) {
    let response = tokio::time::timeout(CONNECTION_TIMEOUT, async {
        let (read_half, mut write_half) = stream.into_split();
        let mut request = String::new();
        let mut reader = AsyncBufReader::new(read_half).take(1024);
        // The client announces itself with one line; anything is answered.
        reader.read_line(&mut request).await?;
        let live = AgentState::lock(state).live_state();
        let mut bytes = serde_json::to_vec(&live)?;
        bytes.push(b'\n');
        write_half.write_all(&bytes).await?;
        write_half.shutdown().await
    })
    .await;
    match response {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log!("live status query failed: {e}"),
        Err(_) => log!("live status query timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(name: &str) -> SpeedPaths {
        let dir = std::env::temp_dir().join(format!("berry-speed-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        SpeedPaths {
            store: dir.join("speed.json"),
            pid: dir.join("speed.pid"),
            socket: dir.join("speed.sock"),
            log: dir.join("speed.log"),
            dir,
        }
    }

    #[test]
    fn plist_points_at_the_agent_subcommand() {
        let text = plist(
            Path::new("/usr/local/bin/berry"),
            Path::new("/tmp/speed.log"),
        );
        assert!(text.contains("<string>com.berry.speed</string>"));
        assert!(text.contains("<string>speed</string>"));
        assert!(text.contains("<string>agent</string>"));
        assert!(text.contains("<key>LimitLoadToSessionType</key>"));
        assert!(text.contains("<string>Aqua</string>"));
        // Restart after a crash, but stay stopped after a clean exit (the
        // menu's Turn off item), which launchd reports as exit status 0.
        assert!(text.contains("<key>KeepAlive</key>"));
        assert!(text.contains("<key>SuccessfulExit</key>"));
        assert!(text.contains("<false/>"));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
        assert_eq!(
            text.matches("<array>").count(),
            text.matches("</array>").count()
        );
    }

    #[test]
    fn plist_escapes_paths() {
        let text = plist(Path::new("/opt/a&b/<berry>"), Path::new("/tmp/x.log"));
        assert!(text.contains("<string>/opt/a&amp;b/&lt;berry&gt;</string>"));
    }

    #[test]
    fn pid_files_round_trip_and_are_exclusive() {
        let paths = paths("pid");
        fs::create_dir_all(&paths.dir).unwrap();
        let me = PidFile {
            pid: 42,
            started_at: 1_700_000_000,
        };
        assert!(PidFile::create(&paths.pid, me).unwrap());
        assert!(!PidFile::create(&paths.pid, me).unwrap());
        assert_eq!(PidFile::read(&paths.pid), Some(me));
        let mode = fs::metadata(&paths.pid).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE);
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn liveness_checks_match_reality() {
        assert!(process_alive(std::process::id()));
        // Pid 0 is the kernel; nobody may signal it, and it is not a process
        // we may treat as an agent.
        assert!(!live_agent(&paths("nobody")).is_some());
    }

    #[test]
    fn stale_pid_files_are_ignored() {
        let paths = paths("stale");
        fs::create_dir_all(&paths.dir).unwrap();
        fs::write(&paths.pid, r#"{"pid": 999999999, "started_at": 0}"#).unwrap();
        assert_eq!(live_agent(&paths), None);
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn foreign_pids_are_not_claimed() {
        let paths = paths("foreign");
        fs::create_dir_all(&paths.dir).unwrap();
        // Pid 1 is launchd, alive but not berry.
        fs::write(&paths.pid, r#"{"pid": 1, "started_at": 0}"#).unwrap();
        if executable_of(1).is_some() {
            assert_eq!(live_agent(&paths), None);
        }
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn sample_once_records_today() {
        let paths = paths("once");
        let live = sample_once(&paths, MIN_INTERVAL_SECONDS).unwrap();
        assert!(live.sample_at > 0);
        let store = UsageStore::load(&paths.store).unwrap().unwrap();
        let today = LocalDate::today();
        assert!(store.day(today).is_some());
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn live_state_round_trips_over_the_socket() {
        let paths = paths("live");
        ensure_dir(&paths).unwrap();
        let state = Arc::new(Mutex::new(AgentState {
            store: UsageStore::default(),
            store_path: paths.store.clone(),
            previous_totals: None,
            last_sample_at: Instant::now(),
            rates: (1234.5, 67.0),
            last_save: Instant::now(),
            dirty: false,
            today: LocalDate::today(),
            started_at: 42,
            interval: 1.0,
            save_errors_logged: false,
        }));

        // The server thread owns the listener and the runtime that drives
        // it; the client below uses the plain, synchronous `live_state`.
        let client_paths = paths.clone();
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let listener = AsyncUnixListener::from_std(bind_socket(&paths).unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                serve_state(&state, stream).await;
            });
        });

        // Wait for the socket to be connectable.
        let deadline = Instant::now() + Duration::from_secs(5);
        let live = loop {
            if let Some(live) = live_state(&client_paths) {
                break live;
            }
            assert!(Instant::now() < deadline, "socket never answered");
            std::thread::sleep(Duration::from_millis(20));
        };
        server.join().unwrap();
        assert_eq!(live.started_at, 42);
        assert_eq!(live.download_bps, 1234.5);
        assert_eq!(live.upload_bps, 67.0);
        assert_eq!(live.interval_seconds, 1.0);
        assert_eq!(live.pid, std::process::id());
        let _ = fs::remove_dir_all(&client_paths.dir);
    }
}
