//! Persistent per-day internet usage.
//!
//! The store is a small JSON document in the user's data directory. It
//! keeps at most [`RETENTION_DAYS`] calendar days (today included);
//! anything older is removed automatically on every prune, which the agent
//! runs whenever it saves.
//!
//! Writes are atomic: the new document goes to a temporary file in the same
//! directory, which is fsynced and renamed over the old one, so a crash or
//! power loss can never leave a half-written file.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::date::LocalDate;

/// Serialized format version.
pub const STORE_VERSION: u32 = 1;

/// Days kept, today included; older records are deleted automatically.
pub const RETENTION_DAYS: i64 = 31;

/// Documents larger than this are treated as corrupt.
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;

const STORE_MODE: u32 = 0o600;

/// One calendar day of traffic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayUsage {
    /// `YYYY-MM-DD`, local time.
    pub date: String,
    pub download_bytes: u64,
    pub upload_bytes: u64,
}

impl DayUsage {
    pub fn total_bytes(&self) -> u64 {
        self.download_bytes.saturating_add(self.upload_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageStore {
    pub version: u32,
    /// Sorted newest first once loaded.
    pub days: Vec<DayUsage>,
}

impl Default for UsageStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            days: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("{} is not valid usage data: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: String },
}

impl UsageStore {
    /// Loads the store. `Ok(None)` means the file does not exist yet.
    pub fn load(path: &Path) -> Result<Option<Self>, StoreError> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        let mut contents = Vec::new();
        file.take(MAX_STORE_BYTES + 1)
            .read_to_end(&mut contents)
            .map_err(|source| StoreError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        if contents.len() as u64 > MAX_STORE_BYTES {
            return Err(StoreError::Corrupt {
                path: path.to_path_buf(),
                reason: format!("file exceeds {MAX_STORE_BYTES} bytes"),
            });
        }
        let store: Self = serde_json::from_slice(&contents).map_err(|e| StoreError::Corrupt {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        if store.version == 0 || store.version > STORE_VERSION {
            return Err(StoreError::Corrupt {
                path: path.to_path_buf(),
                reason: format!(
                    "unsupported version {} (this wattbit understands {STORE_VERSION})",
                    store.version
                ),
            });
        }
        for day in &store.days {
            if LocalDate::parse(&day.date).is_none() {
                return Err(StoreError::Corrupt {
                    path: path.to_path_buf(),
                    reason: format!("invalid date {:?}", day.date),
                });
            }
        }
        Ok(Some(store))
    }

    /// Atomically replaces the on-disk store.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let write_err = |source| StoreError::Write {
            path: path.to_path_buf(),
            source,
        };
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let file_name = path.file_name().ok_or_else(|| {
            write_err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path has no file name",
            ))
        })?;

        let mut json = serde_json::to_vec_pretty(self).map_err(|e| write_err(e.into()))?;
        json.push(b'\n');
        let tmp = dir.join(format!(
            ".{}.tmp.{}",
            file_name.to_string_lossy(),
            std::process::id()
        ));

        let result = (|| -> io::Result<()> {
            match fs::remove_file(&tmp) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(STORE_MODE)
                .open(&tmp)?;
            file.set_permissions(Permissions::from_mode(STORE_MODE))?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            File::open(&dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(write_err)
    }

    /// Moves a corrupt file out of the way (to `NAME.corrupt.UNIXTIME`) so
    /// the agent can start a fresh history instead of failing forever.
    pub fn quarantine(path: &Path) -> io::Result<Option<PathBuf>> {
        if !path.exists() {
            return Ok(None);
        }
        let target = PathBuf::from(format!(
            "{}.corrupt.{}",
            path.display(),
            super::date::now_unix()
        ));
        fs::rename(path, &target)?;
        Ok(Some(target))
    }

    /// Adds a traffic delta to `date`, merging with an existing record.
    pub fn add(&mut self, date: LocalDate, download_bytes: u64, upload_bytes: u64) {
        let iso = date.iso();
        match self.days.iter_mut().find(|day| day.date == iso) {
            Some(day) => {
                day.download_bytes = day.download_bytes.saturating_add(download_bytes);
                day.upload_bytes = day.upload_bytes.saturating_add(upload_bytes);
            }
            None => self.days.push(DayUsage {
                date: iso,
                download_bytes,
                upload_bytes,
            }),
        }
    }

    /// Adds every record of `other` into this store, summing days both
    /// contain. Records with an invalid date are skipped.
    pub fn merge(&mut self, other: &UsageStore) {
        for day in &other.days {
            if let Some(date) = LocalDate::parse(&day.date) {
                self.add(date, day.download_bytes, day.upload_bytes);
            }
        }
    }

    /// Deletes records older than the retention window. Returns how many
    /// were removed.
    pub fn prune(&mut self, today: LocalDate) -> usize {
        let oldest = today.add_days(-(RETENTION_DAYS - 1));
        let before = self.days.len();
        self.days
            .retain(|day| LocalDate::parse(&day.date).is_some_and(|date| date >= oldest));
        before - self.days.len()
    }

    /// Records inside the retention window, newest first.
    pub fn window(&self, today: LocalDate) -> Vec<DayUsage> {
        let oldest = today.add_days(-(RETENTION_DAYS - 1));
        let mut days: Vec<DayUsage> = self
            .days
            .iter()
            .filter(|day| {
                LocalDate::parse(&day.date).is_some_and(|date| date >= oldest && date <= today)
            })
            .cloned()
            .collect();
        days.sort_by(|a, b| b.date.cmp(&a.date));
        days
    }

    /// The record for `date`, if one exists.
    pub fn day(&self, date: LocalDate) -> Option<&DayUsage> {
        let iso = date.iso();
        self.days.iter().find(|day| day.date == iso)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wattbit-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("speed.json");
        let mut store = UsageStore::default();
        store.add(LocalDate::new(2026, 10, 18).unwrap(), 1_000, 200);
        store.save(&path).unwrap();
        let loaded = UsageStore::load(&path).unwrap().unwrap();
        assert_eq!(loaded, store);

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, STORE_MODE);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_loads_as_none() {
        let dir = temp_dir("missing");
        let loaded = UsageStore::load(&dir.join("nope.json")).unwrap();
        assert!(loaded.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_files_are_rejected_and_can_be_quarantined() {
        let dir = temp_dir("corrupt");
        let path = dir.join("speed.json");
        fs::write(&path, b"{not json").unwrap();
        let error = UsageStore::load(&path).unwrap_err();
        assert!(matches!(error, StoreError::Corrupt { .. }));

        let moved = UsageStore::quarantine(&path).unwrap().unwrap();
        assert!(moved.exists());
        assert!(!path.exists());
        assert!(UsageStore::load(&path).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_versions_and_bad_dates_are_corrupt() {
        let dir = temp_dir("version");
        let path = dir.join("speed.json");
        fs::write(&path, br#"{"version": 99, "days": []}"#).unwrap();
        assert!(matches!(
            UsageStore::load(&path).unwrap_err(),
            StoreError::Corrupt { .. }
        ));
        fs::write(
            &path,
            br#"{"version": 1, "days": [{"date": "yesterday", "download_bytes": 1, "upload_bytes": 0}]}"#,
        )
        .unwrap();
        assert!(matches!(
            UsageStore::load(&path).unwrap_err(),
            StoreError::Corrupt { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_fields_default_and_deltas_merge() {
        let mut store: UsageStore =
            serde_json::from_str(r#"{"version": 1, "days": [{"date": "2026-10-18"}]}"#).unwrap();
        assert_eq!(store.days[0].download_bytes, 0);
        store.add(LocalDate::new(2026, 10, 18).unwrap(), 5, 6);
        store.add(LocalDate::new(2026, 10, 18).unwrap(), 7, 8);
        assert_eq!(store.days.len(), 1);
        assert_eq!(
            store.day(LocalDate::new(2026, 10, 18).unwrap()).unwrap(),
            &DayUsage {
                date: "2026-10-18".into(),
                download_bytes: 12,
                upload_bytes: 14,
            }
        );
    }

    #[test]
    fn prune_keeps_exactly_the_retention_window() {
        let today = LocalDate::new(2026, 10, 18).unwrap();
        let mut store = UsageStore::default();
        let kept = today.add_days(-(RETENTION_DAYS - 1));
        store.add(kept, 1, 1); // exactly 31 days including today: kept
        store.add(kept.add_days(-1), 1, 1); // 32 days old: deleted
        store.add(today, 1, 1);
        assert_eq!(store.days.len(), 3);

        let removed = store.prune(today);
        assert_eq!(removed, 1);
        let dates: Vec<&str> = store.days.iter().map(|d| d.date.as_str()).collect();
        assert_eq!(dates, [kept.iso().as_str(), today.iso().as_str()]);

        // A window view is newest-first and excludes future records.
        store.add(today.add_days(1), 9, 9);
        let window = store.window(today);
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].date, today.iso());
    }

    #[test]
    fn quarantine_is_a_no_op_without_a_file() {
        let dir = temp_dir("quarantine");
        assert!(UsageStore::quarantine(&dir.join("none.json"))
            .unwrap()
            .is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
