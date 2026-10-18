//! Human-readable traffic figures, shared by the menu bar and the dashboard.
//!
//! Units are decimal (SI): 1 KB is 1000 bytes, 1 MB is 1000 KB. That is the
//! convention for network throughput, where a "100 Mbps" link moves
//! 100,000,000 bits per second. Small values keep two decimals so that
//! idle trickle traffic stays readable; large ones drop them to keep the
//! menu bar compact.

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// Scales `value` down to the largest unit that keeps it below 1000.
fn scale(value: f64) -> (&'static str, f64) {
    let mut value = value.max(0.0);
    let mut index = 0;
    while value >= 1000.0 && index + 1 < UNITS.len() {
        value /= 1000.0;
        index += 1;
    }
    (UNITS[index], value)
}

/// Decimals rendered for a scaled value: `1.23`, `12.3`, `123`.
fn decimals(scaled: f64) -> usize {
    if scaled == 0.0 {
        0
    } else if scaled < 10.0 {
        2
    } else if scaled < 100.0 {
        1
    } else {
        0
    }
}

fn render(value: f64, suffix: &str) -> String {
    let (unit, scaled) = scale(value);
    if scaled == 0.0 {
        return format!("0 {unit}{suffix}");
    }
    format!("{:.*} {unit}{suffix}", decimals(scaled), scaled)
}

/// A transfer rate such as `1.23 MB/s`, `340 KB/s` or `0 B/s`.
pub fn rate(bytes_per_second: f64) -> String {
    render(bytes_per_second, "/s")
}

/// A data volume such as `12.34 GB` or `0 B`.
pub fn bytes(value: u64) -> String {
    render(value as f64, "")
}

/// `↓ 1.23 MB/s ↑ 340 KB/s`, as shown in the menu bar.
pub fn rates(download_bps: f64, upload_bps: f64) -> String {
    format!("↓ {} ↑ {}", rate(download_bps), rate(upload_bps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_step_through_units() {
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(999.0), "999 B/s");
        assert_eq!(rate(1000.0), "1.00 KB/s");
        assert_eq!(rate(1500.0), "1.50 KB/s");
        assert_eq!(rate(340_000.0), "340 KB/s");
        assert_eq!(rate(1_230_000.0), "1.23 MB/s");
        assert_eq!(rate(12_300_000.0), "12.3 MB/s");
        assert_eq!(rate(123_000_000.0), "123 MB/s");
        assert_eq!(rate(9_990_000_000_000.0), "9.99 TB/s");
        assert_eq!(rate(-5.0), "0 B/s");
    }

    #[test]
    fn volumes_step_through_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_500), "1.50 KB");
        assert_eq!(bytes(234_000_000), "234 MB");
        assert_eq!(bytes(12_340_000_000), "12.3 GB");
        assert_eq!(bytes(1_234_500_000_000), "1.23 TB");
        assert!(!bytes(u64::MAX).is_empty());
    }

    #[test]
    fn the_menu_bar_pair_reads_naturally() {
        assert_eq!(rates(1_230_000.0, 340_000.0), "↓ 1.23 MB/s ↑ 340 KB/s");
        assert_eq!(rates(0.0, 0.0), "↓ 0 B/s ↑ 0 B/s");
    }
}
