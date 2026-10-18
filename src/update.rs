//! Release checks behind the update notice.
//!
//! The speed meter's agent asks GitHub for the latest wattbit release every
//! [`CHECK_INTERVAL`] and caches the answer in the user's data directory.
//! The menu bar item, `wattbit speed status`, `wattbit status` and
//! `wattbit doctor` only ever read that cache, so none of them touch the
//! network or wait on it. The root daemon never checks for updates.
//!
//! Updating is the install command itself ([`INSTALL_COMMAND`]): the
//! installer replaces the binary, restarts the daemon and the meter, and
//! keeps every setting and the usage history.
//!
//! Set `WATTBIT_NO_UPDATE_CHECK=1` to turn the check off; `wattbit speed on`
//! carries the variable into the login item.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

/// GitHub repository wattbit is released from.
pub const REPOSITORY: &str = "rimu-7/wattbit";

/// The one-line installer, which is also the update command.
pub const INSTALL_COMMAND: &str =
    "curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash";

/// Environment variable that turns the release check off.
pub const DISABLE_ENV: &str = "WATTBIT_NO_UPDATE_CHECK";

/// How often the agent asks GitHub for the latest release.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

/// How soon a failed check (offline, rate-limited) is retried.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Delay before the first check after the agent starts, so logging in
/// never waits on the network.
pub const STARTUP_DELAY: Duration = Duration::from_secs(30);

/// Upper bound on one release query, enforced by curl itself.
const FETCH_TIMEOUT_SECONDS: u32 = 20;

/// Cache files larger than this are ignored.
const MAX_CACHE_BYTES: u64 = 4096;

const CURL: &str = "/usr/bin/curl";

/// A `MAJOR.MINOR.PATCH` release number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parses `1.2.3` or `v1.2.3`. Pre-release and build suffixes
    /// (`1.2.3-rc.1`, `1.2.3+abc`) are rejected: only final releases are
    /// offered as updates.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// The version of this build.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION"))
            .expect("Cargo.toml holds a MAJOR.MINOR.PATCH version")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The last answer from GitHub, stored as `update.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCache {
    /// Unix time of the successful check.
    pub checked_at: i64,
    /// The latest release, without a leading `v`.
    pub latest: String,
}

impl UpdateCache {
    /// The cached release, when it is newer than this build.
    pub fn newer_than_current(&self) -> Option<Version> {
        Version::parse(&self.latest).filter(|latest| *latest > Version::current())
    }
}

/// An update this build should tell the user about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateNotice {
    pub current: String,
    pub latest: String,
    /// What to run in Terminal to update.
    pub command: String,
}

impl UpdateNotice {
    pub fn new(latest: Version) -> Self {
        Self {
            current: Version::current().to_string(),
            latest: latest.to_string(),
            command: INSTALL_COMMAND.to_owned(),
        }
    }

    /// Two lines for terminal output: the news, then the command.
    pub fn render(&self) -> String {
        format!(
            "Update available: wattbit {} (you have {}). To update, run:\n  {}\n",
            self.latest, self.current, self.command
        )
    }
}

/// Where the cache lives inside the speed meter's data directory.
pub fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("update.json")
}

/// Reads the cache; a missing or unreadable file is simply no information.
pub fn read_cache(path: &Path) -> Option<UpdateCache> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > MAX_CACHE_BYTES {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Atomically replaces the cache (mode 0600).
pub fn write_cache(path: &Path, cache: &UpdateCache) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let tmp = dir.join(format!(".update.json.tmp.{}", std::process::id()));
    let result = (|| -> io::Result<()> {
        let _ = fs::remove_file(&tmp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut json = serde_json::to_vec(cache)?;
        json.push(b'\n');
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The pending update recorded in `data_dir`, if any. Never touches the
/// network.
pub fn pending(data_dir: &Path) -> Option<UpdateNotice> {
    read_cache(&cache_path(data_dir))?
        .newer_than_current()
        .map(UpdateNotice::new)
}

/// Whether the release check may run.
pub fn checks_enabled() -> bool {
    std::env::var_os(DISABLE_ENV).is_none_or(|value| value.is_empty() || value == "0")
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// Extracts the version from a GitHub "latest release" response.
pub fn parse_release(json: &[u8]) -> anyhow::Result<Version> {
    let release: Release = serde_json::from_slice(json).context("unexpected reply from GitHub")?;
    Version::parse(&release.tag_name)
        .with_context(|| format!("release tag {:?} is not a version", release.tag_name))
}

/// Asks GitHub for the latest release. Blocks for at most
/// [`FETCH_TIMEOUT_SECONDS`]; call it off the UI and sampling paths.
///
/// macOS ships curl with the system's trust store, so this needs no TLS
/// dependency of its own.
pub fn fetch_latest() -> anyhow::Result<Version> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let output = Command::new(CURL)
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--max-time",
            &FETCH_TIMEOUT_SECONDS.to_string(),
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
            concat!("wattbit/", env!("CARGO_PKG_VERSION")),
            &url,
        ])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {CURL}"))?;
    if !output.status.success() {
        bail!(
            "checking {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_release(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn versions_parse_with_or_without_a_v() {
        assert_eq!(Version::parse("1.2.3"), Some(v(1, 2, 3)));
        assert_eq!(Version::parse("v0.10.0"), Some(v(0, 10, 0)));
        assert_eq!(Version::parse(" v2.0.11\n"), Some(v(2, 0, 11)));
    }

    #[test]
    fn malformed_and_prerelease_versions_are_rejected() {
        for text in [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1..3",
            "1.2.3-rc.1",
            "+1.2.3",
            "-1.2.3",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn versions_order_numerically() {
        assert!(v(0, 10, 0) > v(0, 9, 9));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert!(v(0, 2, 1) > v(0, 2, 0));
        assert_eq!(v(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn the_current_version_parses() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn only_newer_releases_are_offered() {
        let current = Version::current();
        let newer = v(current.major, current.minor, current.patch + 1);
        let cache = |latest: String| UpdateCache {
            checked_at: 0,
            latest,
        };
        assert_eq!(cache(newer.to_string()).newer_than_current(), Some(newer));
        assert_eq!(cache(current.to_string()).newer_than_current(), None);
        assert_eq!(cache("0.0.1".into()).newer_than_current(), None);
        assert_eq!(cache("garbage".into()).newer_than_current(), None);
    }

    #[test]
    fn github_replies_are_parsed() {
        let reply = br#"{"tag_name": "v0.3.1", "name": "wattbit 0.3.1", "assets": []}"#;
        assert_eq!(parse_release(reply).unwrap(), v(0, 3, 1));
        assert!(parse_release(br#"{"message": "Not Found"}"#).is_err());
        assert!(parse_release(br#"{"tag_name": "nightly"}"#).is_err());
    }

    #[test]
    fn the_cache_round_trips_and_tolerates_junk() {
        let dir = std::env::temp_dir().join(format!("wattbit-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = cache_path(&dir);
        assert_eq!(read_cache(&path), None);
        assert_eq!(pending(&dir), None);

        let current = Version::current();
        let newer = v(current.major + 1, 0, 0);
        let cache = UpdateCache {
            checked_at: 1_700_000_000,
            latest: newer.to_string(),
        };
        write_cache(&path, &cache).unwrap();
        assert_eq!(read_cache(&path), Some(cache));
        let notice = pending(&dir).unwrap();
        assert_eq!(notice.latest, newer.to_string());
        assert_eq!(notice.command, INSTALL_COMMAND);
        assert!(notice.render().contains(INSTALL_COMMAND));

        fs::write(&path, b"not json").unwrap();
        assert_eq!(read_cache(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
