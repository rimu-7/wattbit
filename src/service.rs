//! LaunchDaemon installation.
//!
//! The binary is copied to a root-owned location before the plist points
//! launchd at it, so the root daemon never executes a user-writable file.
//!
//! Loading uses `/bin/launchctl`: macOS has no supported C API that lets a
//! command-line tool bootstrap a system LaunchDaemon (`SMJobBless` is
//! deprecated and `SMAppService` requires an app bundle). This is the only
//! external process berry runs, and none of the battery functionality
//! depends on it.

use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context};

use crate::conflicts;
use crate::CONFIG_PATH;

pub const LABEL: &str = "com.berry.daemon";
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/com.berry.daemon.plist";
pub const INSTALLED_BINARY: &str = "/Library/PrivilegedHelperTools/berryd";
pub const LOG_PATH: &str = "/var/log/berry.log";
/// Symlink that puts the `berry` command on the default PATH.
pub const CLI_LINK: &str = "/usr/local/bin/berry";

const LAUNCHCTL: &str = "/bin/launchctl";
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(30);

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
        bail!("`berry {command}` must be run as root (use sudo)");
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

fn is_loaded() -> anyhow::Result<bool> {
    Ok(launchctl(&["print", &service_target()])?.status.success())
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

/// Installs the binary and LaunchDaemon plist, then loads the service.
pub fn install() -> anyhow::Result<String> {
    require_root("install-service")?;
    let current = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("locating the running executable")?;
    let installed = PathBuf::from(INSTALLED_BINARY);

    if is_loaded()? {
        // The running daemon restores factory charging on SIGTERM.
        launchctl_checked(&["bootout", &service_target()])?;
    }

    if current != installed {
        let binary =
            fs::read(&current).with_context(|| format!("reading {}", current.display()))?;
        write_root_file(&installed, &binary, 0o755)?;
    }
    write_root_file(Path::new(PLIST_PATH), plist(&installed).as_bytes(), 0o644)?;
    let cli = link_cli()?;

    // A previous `launchctl disable` would make bootstrap fail.
    launchctl_checked(&["enable", &service_target()])?;
    launchctl_checked(&["bootstrap", "system", PLIST_PATH])?;

    let mut summary = format!(
        "Installed {INSTALLED_BINARY} and {PLIST_PATH}; the daemon is running.\n\
         {cli}\n\
         Logs: {LOG_PATH}. Check it with `berry status`."
    );
    let others = conflicts::running();
    if !others.is_empty() {
        summary.push_str(&format!(
            "\n\nWarning: {} also {} charging, so berry stays paused until {} \
             removed. Run `berry doctor` for the exact removal commands.",
            conflicts::summarize(&others),
            conflicts::agree(&others, "controls", "control"),
            conflicts::agree(&others, "it is", "they are"),
        ));
    }
    Ok(summary)
}

/// Links [`CLI_LINK`] to the installed binary so `berry` is on the PATH.
/// An existing file that is not berry's link is left alone.
fn link_cli() -> anyhow::Result<String> {
    let link = Path::new(CLI_LINK);
    match fs::symlink_metadata(link) {
        Ok(meta) => {
            let ours = meta.file_type().is_symlink()
                && fs::read_link(link).is_ok_and(|target| target == Path::new(INSTALLED_BINARY));
            return Ok(if ours {
                format!("The `berry` command is available at {CLI_LINK}.")
            } else {
                format!(
                    "{CLI_LINK} already exists and is not berry's; left it unchanged. \
                     Run berry as {INSTALLED_BINARY} instead."
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
        "The `berry` command is now available at {CLI_LINK}."
    ))
}

/// Removes [`CLI_LINK`] if it is berry's link.
fn unlink_cli() -> anyhow::Result<bool> {
    let link = Path::new(CLI_LINK);
    let ours = fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink())
        && fs::read_link(link).is_ok_and(|target| target == Path::new(INSTALLED_BINARY));
    if ours {
        fs::remove_file(link).with_context(|| format!("removing {CLI_LINK}"))?;
    }
    Ok(ours)
}

/// Stops the service and removes the plist and binary. With `purge`, also
/// deletes the configuration file.
pub fn uninstall(purge: bool) -> anyhow::Result<String> {
    require_root("uninstall-service")?;
    let mut done = Vec::new();
    if is_loaded()? {
        launchctl_checked(&["bootout", &service_target()])?;
        done.push("stopped the daemon (factory charging restored)".to_owned());
    }
    if remove_if_present(Path::new(PLIST_PATH))? {
        done.push(format!("removed {PLIST_PATH}"));
    }
    if unlink_cli()? {
        done.push(format!("removed {CLI_LINK}"));
    }
    if remove_if_present(Path::new(INSTALLED_BINARY))? {
        done.push(format!("removed {INSTALLED_BINARY}"));
    }
    if purge && remove_if_present(Path::new(CONFIG_PATH))? {
        done.push(format!("removed {CONFIG_PATH}"));
    }
    if done.is_empty() {
        return Ok("The berry service was not installed; nothing to do.".into());
    }
    let mut summary = done.join("\n");
    if !purge && Path::new(CONFIG_PATH).exists() {
        summary.push_str(&format!(
            "\nKept {CONFIG_PATH} (pass --purge to delete it)."
        ));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed() {
        let text = plist(Path::new(INSTALLED_BINARY));
        assert!(text.starts_with("<?xml"));
        assert!(text.contains("<string>com.berry.daemon</string>"));
        assert!(text.contains("<string>/Library/PrivilegedHelperTools/berryd</string>"));
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
        let text = plist(Path::new("/opt/a&b/<berry>"));
        assert!(text.contains("<string>/opt/a&amp;b/&lt;berry&gt;</string>"));
    }
}
