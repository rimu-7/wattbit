//! Assembling the `berry speed status` document.
//!
//! The document combines three sources: the pid file (is the meter on?), the
//! live socket (current rates and totals, only while the agent answers) and
//! the usage store (the last [`RETENTION_DAYS`] days).

use serde::Serialize;

use super::agent;
use super::date::LocalDate;
use super::store::{DayUsage, UsageStore, RETENTION_DAYS};

/// Everything `berry speed status` reports.
#[derive(Debug, Clone, Serialize)]
pub struct SpeedStatus {
    /// Whether the sampling agent is running.
    pub running: bool,
    pub pid: Option<u32>,
    /// Unix time the running agent started.
    pub started_at: Option<i64>,
    /// Live figures, when the running agent answered its socket.
    pub live: Option<agent::LiveState>,
    /// The usage history, when it could be read.
    pub usage: Option<UsageReport>,
    /// A problem with the usage file, if any.
    pub warning: Option<String>,
}

/// Traffic over the retention window.
#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub window_start: String,
    pub window_end: String,
    pub retention_days: i64,
    /// Days with at least one recorded answer, newest first.
    pub days: Vec<DayUsage>,
    pub recorded_days: usize,
    pub today: Option<DayUsage>,
    pub total_download_bytes: u64,
    pub total_upload_bytes: u64,
    /// Averages over the recorded days.
    pub average_download_bytes: u64,
    pub average_upload_bytes: u64,
}

/// Reads the current state, tolerating a missing or unreadable store.
pub fn query(paths: &agent::SpeedPaths) -> SpeedStatus {
    let agent = agent::live_agent(paths);
    let live = if agent.is_some() {
        agent::live_state(paths)
    } else {
        None
    };
    let today = LocalDate::today();
    let (usage, warning) = match UsageStore::load(&paths.store) {
        Ok(Some(store)) => (Some(report(&store, today)), None),
        Ok(None) => (None, None),
        Err(e) => (None, Some(e.to_string())),
    };
    SpeedStatus {
        running: agent.is_some(),
        pid: agent.map(|agent| agent.pid),
        started_at: agent.map(|agent| agent.started_at),
        live,
        usage,
        warning,
    }
}

fn report(store: &UsageStore, today: LocalDate) -> UsageReport {
    let days = store.window(today);
    let recorded_days = days.len();
    let total_download_bytes = days
        .iter()
        .fold(0u64, |acc, day| acc.saturating_add(day.download_bytes));
    let total_upload_bytes = days
        .iter()
        .fold(0u64, |acc, day| acc.saturating_add(day.upload_bytes));
    let divisor = u64::try_from(recorded_days.max(1)).unwrap_or(1);
    UsageReport {
        window_start: today.add_days(-(RETENTION_DAYS - 1)).iso(),
        window_end: today.iso(),
        retention_days: RETENTION_DAYS,
        today: store.day(today).cloned(),
        recorded_days,
        total_download_bytes,
        total_upload_bytes,
        average_download_bytes: total_download_bytes / divisor,
        average_upload_bytes: total_upload_bytes / divisor,
        days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn paths(name: &str) -> agent::SpeedPaths {
        let dir = std::env::temp_dir().join(format!("berry-status-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        agent::SpeedPaths {
            store: dir.join("speed.json"),
            pid: dir.join("speed.pid"),
            socket: dir.join("speed.sock"),
            log: dir.join("speed.log"),
            dir,
        }
    }

    #[test]
    fn missing_everything_reports_off() {
        let status = query(&paths("off"));
        assert!(!status.running);
        assert!(status.live.is_none());
        assert!(status.usage.is_none());
        assert!(status.warning.is_none());
    }

    #[test]
    fn usage_windows_are_summarized() {
        let paths = paths("usage");
        let today = LocalDate::today();
        let mut store = UsageStore::default();
        store.add(today, 1_000, 100);
        store.add(today.add_days(-1), 2_000, 200);
        store.add(today.add_days(-40), 9_999, 9_999); // outside the window
        store.save(&paths.store).unwrap();

        let status = query(&paths);
        let usage = status.usage.unwrap();
        assert_eq!(usage.recorded_days, 2);
        assert_eq!(usage.total_download_bytes, 3_000);
        assert_eq!(usage.total_upload_bytes, 300);
        assert_eq!(usage.average_download_bytes, 1_500);
        assert_eq!(usage.days[0].date, today.iso());
        assert_eq!(usage.window_end, today.iso());
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn a_corrupt_store_is_reported_not_fatal() {
        let paths = paths("corrupt");
        fs::write(&paths.store, b"nonsense").unwrap();
        let status = query(&paths);
        assert!(status.usage.is_none());
        assert!(status.warning.is_some());
        let _ = fs::remove_dir_all(&paths.dir);
    }
}
