//! LaunchDaemon installation.
//!
//! The binary is copied to a root-owned location before the plist points
//! launchd at it, so the root daemon never executes a user-writable file.
//!
//! Loading uses `/bin/launchctl`: macOS has no supported C API that lets a
//! command-line tool bootstrap a system LaunchDaemon (`SMJobBless` is
//! deprecated and `SMAppService` requires an app bundle). None of the
//! battery functionality depends on it.
//!
//! Installing doubles as updating: the old daemon is stopped (restoring
//! factory charging), the new binary takes its place, and the install only
//! reports success once the new daemon answers on its socket.

use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context};

use crate::client::Client;
use crate::legacy::{self, OldSettings};
use crate::protocol::{Call, DaemonDiagnostics, Reply};
use crate::speed::agent;
use crate::{conflicts, daemon, CONFIG_PATH, SOCKET_PATH};

pub const LABEL: &str = "com.wattbit.daemon";
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/com.wattbit.daemon.plist";
pub const INSTALLED_BINARY: &str = "/Library/PrivilegedHelperTools/wattbitd";
pub const LOG_PATH: &str = "/var/log/wattbit.log";
/// Symlink that puts the `wattbit` command on the default PATH.
pub const CLI_LINK: &str = "/usr/local/bin/wattbit";

const LAUNCHCTL: &str = "/bin/launchctl";
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a booted-out job may stay listed before giving up on it.
const BOOTOUT_TIMEOUT: Duration = Duration::from_secs(15);

/// `launchctl bootstrap` fails with "5: Input/output error" while launchd
/// is still tearing down a job of the same label, which is common right
/// after an update's bootout. It is retried this many times, a second apart.
const BOOTSTRAP_ATTEMPTS: u32 = 5;

/// How long a freshly started daemon may take to probe the hardware and
/// open its socket.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Renders the LaunchDaemon property list for `binary`.
pub fn plist(binary: &Path) -> String {
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
		<string>daemon</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(LABEL),
        binary = xml_escape(&binary.to_string_lossy()),
        log = xml_escape(LOG_PATH),
    )
}

/// Escapes the five XML special characters, for embedding text in the
/// launchd plist (shared with the speed agent's own plist writer).
pub(crate) fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn require_root(command: &str) -> anyhow::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        bail!("`wattbit {command}` must be run as root (use sudo)");
    }
    Ok(())
}

/// Runs launchctl with a bounded wait.
pub(crate) fn launchctl(args: &[&str]) -> anyhow::Result<Output> {
    let child = Command::new(LAUNCHCTL)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {LAUNCHCTL}"))?;
    let pid = child.id() as libc::pid_t;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(LAUNCHCTL_TIMEOUT) {
        Ok(output) => Ok(output?),
        Err(_) => {
            // SAFETY: signalling our own child process.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            bail!("`launchctl {}` timed out", args.join(" "))
        }
    }
}

pub(crate) fn launchctl_checked(args: &[&str]) -> anyhow::Result<()> {
    let output = launchctl(args)?;
    if !output.status.success() {
        bail!(
            "`launchctl {}` failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn service_target() -> String {
    format!("system/{LABEL}")
}

/// Whether launchd knows the job `target` (`system/LABEL`, `gui/UID/LABEL`).
pub(crate) fn loaded(target: &str) -> anyhow::Result<bool> {
    Ok(launchctl(&["print", target])?.status.success())
}

/// Unloads `target` and waits until launchd has let go of it, so a job of
/// the same label can be bootstrapped right away.
pub(crate) fn bootout(target: &str) -> anyhow::Result<()> {
    let output = launchctl(&["bootout", target])?;
    let deadline = std::time::Instant::now() + BOOTOUT_TIMEOUT;
    while loaded(target)? {
        if std::time::Instant::now() >= deadline {
            bail!(
                "`launchctl bootout {target}` did not unload the job ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Loads `plist` into `domain`, retrying while launchd finishes unloading
/// an earlier instance.
pub(crate) fn bootstrap(domain: &str, plist: &str) -> anyhow::Result<()> {
    let mut attempt = 1;
    loop {
        match launchctl_checked(&["bootstrap", domain, plist]) {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= BOOTSTRAP_ATTEMPTS => return Err(e),
            Err(_) => {
                attempt += 1;
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Waits for the daemon to answer on its socket.
fn wait_for_daemon(timeout: Duration) -> anyhow::Result<DaemonDiagnostics> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = Client::new(SOCKET_PATH);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let last_error = match runtime.block_on(client.call(Call::Doctor)) {
            Ok(Reply::Diagnostics(diagnostics)) => return Ok(*diagnostics),
            Ok(_) => "unexpected reply".to_owned(),
            Err(e) => e.to_string(),
        };
        if std::time::Instant::now() >= deadline {
            bail!(
                "the service is installed, but the daemon did not answer within {} s \
                 ({last_error}). See the log: tail -50 {LOG_PATH}",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Ensures `dir` exists and only root can write to it.
fn ensure_root_directory(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, Permissions::from_mode(0o755))?;
    }
    let meta = fs::metadata(dir)?;
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!(
            "{} must be owned by root and not group/world writable; refusing to install there",
            dir.display()
        );
    }
    Ok(())
}

/// Atomically writes `contents` to `path` as a root:wheel file with `mode`.
fn write_root_file(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    ensure_root_directory(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let result = (|| -> io::Result<()> {
        let _ = fs::remove_file(&tmp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.set_permissions(Permissions::from_mode(mode))?;
        std::os::unix::fs::fchown(&file, Some(0), Some(0))?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", path.display()))
}

fn remove_if_present(path: &Path) -> anyhow::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Installs (or updates) the binary and LaunchDaemon plist, moves a berry
/// installation over, then loads the service and waits for it to answer.
pub fn install() -> anyhow::Result<String> {
    require_root("install-service")?;
    let current = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("locating the running executable")?;
    let installed = PathBuf::from(INSTALLED_BINARY);

    let target = service_target();
    if loaded(&target)? {
        // The running daemon restores factory charging on SIGTERM.
        bootout(&target)?;
    }
    // Before the new daemon starts, so it starts with the old settings and
    // never runs alongside berryd.
    let migrated = legacy::clean_system(OldSettings::MoveTo(Path::new(CONFIG_PATH)))?;

    if current != installed {
        let binary =
            fs::read(&current).with_context(|| format!("reading {}", current.display()))?;
        write_root_file(&installed, &binary, 0o755)?;
    }
    write_root_file(Path::new(PLIST_PATH), plist(&installed).as_bytes(), 0o644)?;
    let cli = link_cli()?;

    // A previous `launchctl disable` would make bootstrap fail.
    launchctl_checked(&["enable", &target])?;
    bootstrap("system", PLIST_PATH)?;
    let _ = launchctl(&["kickstart", "-k", &target]);
    let daemon = wait_for_daemon(DAEMON_READY_TIMEOUT)?;

    // If installed via sudo from a user session that had the speed meter running,
    // kickstart the speed agent so it immediately runs the updated binary.
    if let Ok(sudo_uid) = std::env::var("SUDO_UID") {
        let speed_target = format!("gui/{sudo_uid}/com.wattbit.speed");
        if loaded(&speed_target).unwrap_or(false) {
            let _ = launchctl(&["kickstart", "-k", &speed_target]);
        }
    }

    let mut summary = format!(
        "wattbit {} is installed and its daemon is running (pid {}).\n\
         {cli}\n\
         Logs: {LOG_PATH}. Check it with `wattbit status`.",
        daemon.version, daemon.pid
    );
    if daemon.version != env!("CARGO_PKG_VERSION") {
        summary.push_str(&format!(
            "\n\nWarning: this installer is wattbit {}, but the daemon reports {}.",
            env!("CARGO_PKG_VERSION"),
            daemon.version
        ));
    }
    if !migrated.is_empty() {
        summary.push_str("\n\nMoved over from berry (wattbit's old name):");
        for line in &migrated {
            summary.push_str(&format!("\n  - {line}"));
        }
    }
    let others = conflicts::running();
    if !others.is_empty() {
        summary.push_str(&format!(
            "\n\nWarning: {} also {} charging, so wattbit stays paused until {} \
             removed. Run `wattbit doctor` for the exact removal commands.",
            conflicts::summarize(&others),
            conflicts::agree(&others, "controls", "control"),
            conflicts::agree(&others, "it is", "they are"),
        ));
    }
    Ok(summary)
}

/// Links [`CLI_LINK`] to the installed binary so `wattbit` is on the PATH.
/// An existing file that is not wattbit's link is left alone.
fn link_cli() -> anyhow::Result<String> {
    let link = Path::new(CLI_LINK);
    match fs::symlink_metadata(link) {
        Ok(meta) => {
            let ours = meta.file_type().is_symlink()
                && fs::read_link(link).is_ok_and(|target| target == Path::new(INSTALLED_BINARY));
            return Ok(if ours {
                format!("The `wattbit` command is available at {CLI_LINK}.")
            } else {
                format!(
                    "{CLI_LINK} already exists and is not wattbit's; left it unchanged. \
                     Run wattbit as {INSTALLED_BINARY} instead."
                )
            });
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("inspecting {CLI_LINK}")),
    }
    let dir = link.parent().context("link path has no parent")?;
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, Permissions::from_mode(0o755))?;
    }
    std::os::unix::fs::symlink(INSTALLED_BINARY, link)
        .with_context(|| format!("linking {CLI_LINK}"))?;
    Ok(format!(
        "The `wattbit` command is now available at {CLI_LINK}."
    ))
}

/// Removes [`CLI_LINK`] if it is wattbit's link.
fn unlink_cli() -> anyhow::Result<bool> {
    let link = Path::new(CLI_LINK);
    let ours = fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink())
        && fs::read_link(link).is_ok_and(|target| target == Path::new(INSTALLED_BINARY));
    if ours {
        fs::remove_file(link).with_context(|| format!("removing {CLI_LINK}"))?;
    }
    Ok(ours)
}

/// Removes wattbit from this Mac: stops the daemon, puts the charging
/// hardware back to factory behaviour, deletes the daemon's files and
/// stops every user's speed meter, berry's (wattbit's old name) included.
/// With `purge`, also deletes the settings and every user's usage history.
pub fn uninstall(purge: bool) -> anyhow::Result<String> {
    require_root("uninstall-service")?;
    let mut done = Vec::new();
    let mut problems = Vec::new();
    let target = service_target();
    let running = loaded(&target)?;
    let was_installed = running
        || [PLIST_PATH, INSTALLED_BINARY]
            .iter()
            .any(|path| fs::symlink_metadata(path).is_ok());
    if running {
        // The daemon restores factory charging on SIGTERM.
        bootout(&target)?;
        done.push("stopped the daemon".to_owned());
    }
    // First, so launchd can never start the daemon again.
    if remove_if_present(Path::new(PLIST_PATH))? {
        done.push(format!("removed {PLIST_PATH}"));
    }
    let old = legacy::clean_system(if purge {
        OldSettings::Delete
    } else {
        OldSettings::Keep
    })?;
    let had_berry = !old.is_empty();
    done.extend(old);

    // A daemon that crashed, or that launchd had to kill, restored nothing,
    // so restore here as well; after a clean stop this changes nothing.
    if was_installed || had_berry {
        match daemon::restore_factory() {
            Ok(errors) if errors.is_empty() => {
                done.push("restored factory charging behaviour".to_owned())
            }
            Ok(errors) => problems.push(format!(
                "could not fully restore factory charging ({}); restart the Mac to reset it",
                errors.join("; ")
            )),
            Err(e) => problems.push(format!(
                "could not check the charging hardware ({e:#}); restart the Mac to reset it"
            )),
        }
    }

    if unlink_cli()? {
        done.push(format!("removed {CLI_LINK}"));
    }
    if remove_if_present(Path::new(INSTALLED_BINARY))? {
        done.push(format!("removed {INSTALLED_BINARY}"));
    }
    let stale_socket = fs::symlink_metadata(SOCKET_PATH).is_ok_and(|meta| {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_socket()
    });
    if stale_socket && remove_if_present(Path::new(SOCKET_PATH))? {
        done.push(format!("removed {SOCKET_PATH}"));
    }
    if remove_if_present(Path::new(LOG_PATH))? {
        done.push(format!("removed {LOG_PATH}"));
    }
    if purge && remove_if_present(Path::new(CONFIG_PATH))? {
        done.push(format!("removed {CONFIG_PATH}"));
    }
    // Interrupted atomic writes (`.<name>.tmp.<pid>`).
    let mut written = vec![INSTALLED_BINARY, PLIST_PATH];
    if purge {
        written.push(CONFIG_PATH);
    }
    for path in written {
        for tmp in temp_files_of(Path::new(path)) {
            if remove_if_present(&tmp)? {
                done.push(format!("removed {}", tmp.display()));
            }
        }
    }

    done.extend(agent::remove_for_all_users(purge));

    if done.is_empty() && problems.is_empty() {
        return Ok("wattbit is not installed; nothing to do.".into());
    }
    let mut summary = done.join("\n");
    if fs::symlink_metadata(CLI_LINK).is_ok() {
        summary.push_str(&format!(
            "\nLeft {CLI_LINK} in place: it is not wattbit's link."
        ));
    }
    if !purge {
        summary.push_str(&format!(
            "\nKept your settings ({CONFIG_PATH}) and usage history; pass --purge to delete them."
        ));
    }
    for problem in problems {
        summary.push_str(&format!("\nWarning: {problem}"));
    }
    Ok(summary)
}

/// Leftovers of interrupted atomic writes to `path`.
fn temp_files_of(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let prefix = format!(".{}.tmp.", name.to_string_lossy());
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed() {
        let text = plist(Path::new(INSTALLED_BINARY));
        assert!(text.starts_with("<?xml"));
        assert!(text.contains("<string>com.wattbit.daemon</string>"));
        assert!(text.contains("<string>/Library/PrivilegedHelperTools/wattbitd</string>"));
        assert!(text.contains("<string>daemon</string>\n\t\t<string>run</string>"));
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
        let text = plist(Path::new("/opt/a&b/<wattbit>"));
        assert!(text.contains("<string>/opt/a&amp;b/&lt;wattbit&gt;</string>"));
    }
}
