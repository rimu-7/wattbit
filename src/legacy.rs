//! Moving off berry, wattbit's previous name.
//!
//! Every installed path changed with the rename. A berry daemon left
//! running would drive the same SMC keys as wattbitd, so installing wattbit
//! stops it, carries its settings and the speed meter's usage history over,
//! and deletes what is left. Only files berry is known to have created are
//! removed; anything unexpected is reported and left alone.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::Context;

use crate::config::ConfigStore;
use crate::service;
use crate::speed::agent::{self, SpeedPaths};
use crate::speed::store::UsageStore;

pub const DAEMON_LABEL: &str = "com.berry.daemon";
pub const DAEMON_PLIST: &str = "/Library/LaunchDaemons/com.berry.daemon.plist";
pub const DAEMON_BINARY: &str = "/Library/PrivilegedHelperTools/berryd";
pub const CLI_LINK: &str = "/usr/local/bin/berry";
pub const CONFIG: &str = "/etc/berry.json";
pub const SOCKET: &str = "/var/run/berry.sock";
pub const LOG: &str = "/var/log/berry.log";

pub const SPEED_LABEL: &str = "com.berry.speed";
/// The speed meter's data directory, relative to the user's home.
pub const SPEED_DIR: &str = "Library/Application Support/berry";
/// Files the berry speed meter kept in [`SPEED_DIR`].
const SPEED_FILES: &[&str] = &["speed.json", "speed.pid", "speed.sock", "speed.log"];

/// What to do with `/etc/berry.json`.
#[derive(Debug, Clone, Copy)]
pub enum OldSettings<'a> {
    /// Move it to wattbit's config path, unless wattbit already has one.
    MoveTo(&'a Path),
    /// Leave it where it is.
    Keep,
    /// Delete it.
    Delete,
}

/// berry's system files that still exist.
pub fn system_leftovers() -> Vec<&'static str> {
    [DAEMON_PLIST, DAEMON_BINARY, CLI_LINK, CONFIG, SOCKET, LOG]
        .into_iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .collect()
}

/// Stops the berry daemon and removes its files (requires root). Returns
/// one line per action taken.
pub fn clean_system(settings: OldSettings) -> anyhow::Result<Vec<String>> {
    let mut done = Vec::new();
    let target = format!("system/{DAEMON_LABEL}");
    if service::loaded(&target)? {
        // berryd restores factory charging when it is stopped.
        service::bootout(&target)?;
        done.push("stopped the old berry daemon".to_owned());
    }

    match settings {
        OldSettings::MoveTo(config) if Path::new(CONFIG).exists() => {
            if config.exists() {
                remove(
                    Path::new(CONFIG),
                    &mut done,
                    "superseded by wattbit's settings",
                )?;
            } else {
                match ConfigStore::new(CONFIG).load() {
                    Ok(Some(old)) => {
                        ConfigStore::new(config).save(&old).with_context(|| {
                            format!("moving your settings to {}", config.display())
                        })?;
                        fs::remove_file(CONFIG).with_context(|| format!("removing {CONFIG}"))?;
                        done.push(format!(
                            "moved your settings from {CONFIG} to {}",
                            config.display()
                        ));
                    }
                    Ok(None) => {}
                    // Never delete settings that could not be carried over.
                    Err(e) => done.push(format!("kept {CONFIG}: {e}")),
                }
            }
        }
        OldSettings::MoveTo(_) | OldSettings::Keep => {}
        OldSettings::Delete => remove(Path::new(CONFIG), &mut done, "")?,
    }

    if fs::symlink_metadata(SOCKET).is_ok_and(|meta| {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_socket()
    }) {
        remove(Path::new(SOCKET), &mut done, "")?;
    }
    remove(Path::new(DAEMON_PLIST), &mut done, "")?;
    let link_is_berrys = fs::symlink_metadata(CLI_LINK).is_ok_and(|m| m.file_type().is_symlink())
        && fs::read_link(CLI_LINK).is_ok_and(|target| target == Path::new(DAEMON_BINARY));
    if link_is_berrys {
        remove(Path::new(CLI_LINK), &mut done, "")?;
    }
    remove(Path::new(DAEMON_BINARY), &mut done, "")?;
    remove(Path::new(LOG), &mut done, "")?;
    Ok(done)
}

fn remove(path: &Path, done: &mut Vec<String>, why: &str) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            done.push(if why.is_empty() {
                format!("removed {}", path.display())
            } else {
                format!("removed {} ({why})", path.display())
            });
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Stops a berry speed meter, moves its usage history into `paths` and
/// removes its files (runs as the user whose home is `home`). Best effort:
/// problems are reported, never fatal, because they must not keep the new
/// meter from starting. Returns one line per action taken.
pub fn clean_user(home: &Path, paths: &SpeedPaths) -> Vec<String> {
    let mut done = Vec::new();
    let old_dir = home.join(SPEED_DIR);
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{SPEED_LABEL}.plist"));

    let plist_is_berrys = fs::read_to_string(&plist)
        .is_ok_and(|text| text.contains(&format!("<string>{SPEED_LABEL}</string>")));
    if plist_is_berrys {
        // SAFETY: geteuid has no preconditions.
        let target = format!("gui/{}/{SPEED_LABEL}", unsafe { libc::geteuid() });
        if service::loaded(&target).unwrap_or(false) {
            match service::bootout(&target) {
                Ok(()) => done.push("stopped the old berry speed meter".to_owned()),
                Err(e) => done.push(format!("could not stop the old berry login item: {e:#}")),
            }
        }
        match fs::remove_file(&plist) {
            Ok(()) => done.push(format!("removed {}", plist.display())),
            Err(e) => done.push(format!("could not remove {}: {e}", plist.display())),
        }
    }

    // A meter started with `--no-launchd` has no login item; find it by its
    // pid file, and only stop it if it really is berry.
    let old_pid = fs::read_to_string(old_dir.join("speed.pid"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| json["pid"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok());
    if let Some(pid) = old_pid {
        let is_berry = agent::executable_of(pid).is_some_and(|exe| {
            matches!(
                exe.file_name().and_then(|name| name.to_str()),
                Some("berry" | "berryd")
            )
        });
        if is_berry && agent::process_alive(pid) {
            if agent::terminate(pid, std::time::Duration::from_secs(5)) {
                done.push(format!("stopped the old berry speed meter (pid {pid})"));
            } else {
                done.push(format!(
                    "the old berry speed meter (pid {pid}) did not stop"
                ));
            }
        }
    }

    let old_store = old_dir.join("speed.json");
    match UsageStore::load(&old_store) {
        Ok(Some(old)) => {
            let merged = match UsageStore::load(&paths.store) {
                Ok(Some(mut current)) => {
                    current.merge(&old);
                    current
                }
                Ok(None) => old,
                Err(e) => {
                    // The current history is unreadable; keep the old one
                    // in place rather than risk losing both.
                    done.push(format!("kept {}: {e}", old_store.display()));
                    return done;
                }
            };
            let saved = agent::ensure_dir(paths)
                .map_err(|e| format!("{e:#}"))
                .and_then(|()| merged.save(&paths.store).map_err(|e| e.to_string()));
            match saved {
                Ok(()) => {
                    let _ = fs::remove_file(&old_store);
                    done.push(format!(
                        "moved your berry usage history into {}",
                        paths.store.display()
                    ));
                }
                Err(e) => {
                    done.push(format!("kept {}: {e}", old_store.display()));
                    return done;
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            done.push(format!("kept {}: {e}", old_store.display()));
            return done;
        }
    }

    if old_dir.is_dir() {
        for name in SPEED_FILES {
            let _ = fs::remove_file(old_dir.join(name));
        }
        // Quarantined histories and interrupted saves.
        if let Ok(entries) = fs::read_dir(&old_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("speed.json.corrupt.") || name.starts_with(".speed.json.tmp.") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        match fs::remove_dir(&old_dir) {
            Ok(()) => done.push(format!("removed {}", old_dir.display())),
            Err(_) => done.push(format!(
                "left {} in place: it holds files wattbit did not create",
                old_dir.display()
            )),
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speed::date::LocalDate;
    use std::path::PathBuf;

    /// A private home directory, so nothing here can reach the real
    /// `~/Library` or the user's launchd session.
    struct Sandbox {
        home: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let home =
                std::env::temp_dir().join(format!("wattbit-legacy-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            Self { home }
        }

        fn clean(&self) -> Vec<String> {
            clean_user(&self.home, &self.paths())
        }

        fn paths(&self) -> SpeedPaths {
            let dir = self.home.join("Library/Application Support/wattbit");
            SpeedPaths {
                store: dir.join("speed.json"),
                pid: dir.join("speed.pid"),
                socket: dir.join("speed.sock"),
                log: dir.join("speed.log"),
                dir,
            }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn a_clean_machine_needs_nothing() {
        assert!(Sandbox::new("clean").clean().is_empty());
    }

    #[test]
    fn berry_speed_history_is_merged_and_its_files_removed() {
        let sandbox = Sandbox::new("merge");
        let paths = sandbox.paths();
        let today = LocalDate::today();
        let old_dir = sandbox.home.join(SPEED_DIR);
        fs::create_dir_all(&old_dir).unwrap();
        let mut old = UsageStore::default();
        old.add(today, 1_000, 10);
        old.add(today.add_days(-1), 500, 5);
        old.save(&old_dir.join("speed.json")).unwrap();
        fs::write(old_dir.join("speed.log"), b"old log").unwrap();
        // A stale pid file pointing at a process that is not berry.
        fs::write(old_dir.join("speed.pid"), r#"{"pid":1,"started_at":0}"#).unwrap();

        agent::ensure_dir(&paths).unwrap();
        let mut current = UsageStore::default();
        current.add(today, 2_000, 20);
        current.save(&paths.store).unwrap();

        let done = sandbox.clean();
        assert!(
            done.iter()
                .any(|line| line.contains("moved your berry usage history")),
            "{done:?}"
        );
        assert!(
            done.iter().any(|line| line.starts_with("removed ")),
            "{done:?}"
        );
        assert!(!old_dir.exists(), "the old directory should be gone");

        let merged = UsageStore::load(&paths.store).unwrap().unwrap();
        let day = merged.day(today).unwrap();
        assert_eq!((day.download_bytes, day.upload_bytes), (3_000, 30));
        assert_eq!(merged.day(today.add_days(-1)).unwrap().download_bytes, 500);

        // Running it again finds nothing left to do.
        assert!(sandbox.clean().is_empty());
    }

    #[test]
    fn unknown_files_keep_the_old_directory() {
        let today = LocalDate::today();
        let mut old = UsageStore::default();
        old.add(today, 1_000, 10);
        let sandbox = Sandbox::new("foreign");
        let paths = sandbox.paths();
        let old_dir = sandbox.home.join(SPEED_DIR);
        fs::create_dir_all(&old_dir).unwrap();
        old.save(&old_dir.join("speed.json")).unwrap();
        fs::write(old_dir.join("notes.txt"), b"not berry's").unwrap();
        let done = sandbox.clean();
        assert!(done.iter().any(|line| line.contains("left ")), "{done:?}");
        assert!(old_dir.join("notes.txt").exists());
        assert!(!old_dir.join("speed.json").exists());
        assert!(paths.store.exists());
    }
}
