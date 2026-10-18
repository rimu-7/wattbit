//! The `wattbit speed status` dashboard.

use std::fmt::Write as _;

use super::date::format_local;
use super::format;
use super::status::SpeedStatus;

/// Renders the status document as plain text.
pub fn render(status: &SpeedStatus) -> String {
    let mut out = String::new();

    out.push_str("Internet speed meter: ");
    if !status.running {
        out.push_str("off\n");
    } else {
        match (status.pid, status.started_at) {
            (Some(pid), Some(started_at)) => {
                let _ = writeln!(out, "on (pid {pid}, since {})", format_local(started_at));
            }
            _ => out.push_str("on\n"),
        }
    }
    if status.running {
        match &status.live {
            Some(live) => {
                let _ = writeln!(
                    out,
                    "  Live speed: ↓ {}  ↑ {}",
                    format::rate(live.download_bps),
                    format::rate(live.upload_bps)
                );
            }
            None => out.push_str("  Live speed: unavailable (the agent is starting or busy)\n"),
        }
    }

    out.push('\n');
    match (&status.usage, &status.warning) {
        (Some(usage), _) => render_usage(&mut out, usage, status.running),
        (None, Some(warning)) => {
            let _ = writeln!(out, "Usage: unavailable: {warning}");
        }
        (None, None) if status.running => {
            out.push_str("Usage: nothing recorded yet; the meter saves as traffic arrives.\n");
        }
        (None, None) => {
            out.push_str(
                "Usage: nothing recorded yet. Turn the meter on with `wattbit speed on`.\n",
            );
        }
    }

    if !status.running {
        out.push_str("\nThe meter is off, so no usage is being recorded.\n");
    }
    if let Some(notice) = &status.update {
        out.push('\n');
        out.push_str(&notice.render());
    }
    out
}

fn render_usage(out: &mut String, usage: &super::status::UsageReport, running: bool) {
    let _ = writeln!(
        out,
        "Internet usage, last {} days ({} to {}):",
        usage.retention_days, usage.window_start, usage.window_end
    );
    let today = usage
        .today
        .clone()
        .unwrap_or_else(|| super::store::DayUsage {
            date: usage.window_end.clone(),
            download_bytes: 0,
            upload_bytes: 0,
        });
    let _ = writeln!(
        out,
        "  Today:  ↓ {}  ↑ {}",
        format::bytes(today.download_bytes),
        format::bytes(today.upload_bytes)
    );
    let _ = writeln!(
        out,
        "  Total:  ↓ {}  ↑ {}",
        format::bytes(usage.total_download_bytes),
        format::bytes(usage.total_upload_bytes)
    );
    if usage.recorded_days > 0 {
        let _ = writeln!(
            out,
            "  Average: ↓ {}  ↑ {}  per recorded day ({} of {})",
            format::bytes(usage.average_download_bytes),
            format::bytes(usage.average_upload_bytes),
            usage.recorded_days,
            usage.retention_days
        );
    }

    if usage.days.is_empty() {
        return;
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "  {:<10}  {:>10}  {:>10}  {:>10}",
        "Date", "Download", "Upload", "Total"
    );
    for day in &usage.days {
        let _ = writeln!(
            out,
            "  {:<10}  {:>10}  {:>10}  {:>10}",
            day.date,
            format::bytes(day.download_bytes),
            format::bytes(day.upload_bytes),
            format::bytes(day.total_bytes())
        );
    }
    let note = if running {
        "Records older than this window are deleted automatically."
    } else {
        "Records older than this window were deleted automatically."
    };
    let _ = writeln!(out, "\n{note}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speed::agent::LiveState;
    use crate::speed::status::{SpeedStatus, UsageReport};
    use crate::speed::store::DayUsage;

    fn day(date: &str, down: u64, up: u64) -> DayUsage {
        DayUsage {
            date: date.into(),
            download_bytes: down,
            upload_bytes: up,
        }
    }

    #[test]
    fn an_off_meter_renders_its_history() {
        let status = SpeedStatus {
            running: false,
            pid: None,
            started_at: None,
            live: None,
            usage: Some(UsageReport {
                window_start: "2026-09-18".into(),
                window_end: "2026-10-18".into(),
                retention_days: 31,
                days: vec![
                    day("2026-10-18", 1_500, 500),
                    day("2026-10-17", 3_000, 1_000),
                ],
                recorded_days: 2,
                today: Some(day("2026-10-18", 1_500, 500)),
                total_download_bytes: 4_500,
                total_upload_bytes: 1_500,
                average_download_bytes: 2_250,
                average_upload_bytes: 750,
            }),
            warning: None,
            update: None,
        };
        let text = render(&status);
        assert!(text.contains("Internet speed meter: off"));
        assert!(text.contains("Internet usage, last 31 days (2026-09-18 to 2026-10-18):"));
        assert!(text.contains("Today:  ↓ 1.50 KB  ↑ 500 B"));
        assert!(text.contains("Total:  ↓ 4.50 KB  ↑ 1.50 KB"));
        assert!(text.contains("2026-10-17"));
        assert!(text.contains("deleted automatically"));
    }

    #[test]
    fn an_on_meter_shows_live_rates() {
        let status = SpeedStatus {
            running: true,
            pid: Some(4711),
            started_at: Some(1_700_000_000),
            live: Some(LiveState {
                pid: 4711,
                started_at: 1_700_000_000,
                sample_at: 1_700_000_100,
                interval_seconds: 1.0,
                download_bps: 1_230_000.0,
                upload_bps: 340_000.0,
                today_download_bytes: 5_000_000,
                today_upload_bytes: 500_000,
                window_download_bytes: 5_000_000,
                window_upload_bytes: 500_000,
            }),
            usage: None,
            warning: None,
            update: None,
        };
        let text = render(&status);
        assert!(text.contains("Internet speed meter: on (pid 4711, since "));
        assert!(text.contains("Live speed: ↓ 1.23 MB/s  ↑ 340 KB/s"));
        assert!(text.contains("nothing recorded yet"));
        assert!(!text.contains("The meter is off"));
    }

    #[test]
    fn problems_are_surfaced() {
        let status = SpeedStatus {
            running: false,
            pid: None,
            started_at: None,
            live: None,
            usage: None,
            warning: Some("cannot read /no/such/file: boom".into()),
            update: None,
        };
        let text = render(&status);
        assert!(text.contains("Usage: unavailable: cannot read"));
        assert!(!text.contains("Update available"));
    }

    #[test]
    fn updates_come_with_the_command() {
        let status = SpeedStatus {
            running: false,
            pid: None,
            started_at: None,
            live: None,
            usage: None,
            warning: None,
            update: Some(crate::update::UpdateNotice {
                current: "0.3.0".into(),
                latest: "0.4.0".into(),
                command: crate::update::INSTALL_COMMAND.into(),
            }),
        };
        let text = render(&status);
        assert!(text.ends_with(&format!(
            "\nUpdate available: wattbit 0.4.0 (you have 0.3.0). To update, run:\n  {}\n",
            crate::update::INSTALL_COMMAND
        )));
    }
}
