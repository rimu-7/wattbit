//! The internet speed meter (`berry speed`).
//!
//! Unlike charge control, the speed meter needs no root and never talks to
//! the SMC. `berry speed on` starts a small agent in the user's login
//! session that:
//!
//! - samples the kernel's per-interface byte counters once per second,
//! - shows the live rates as a menu bar item,
//! - accumulates traffic into a per-day usage history, kept for 31 days,
//! - answers live-status queries on a private Unix socket, and
//! - stops when told to (`berry speed off` or the menu's *Turn off* item).
//!
//! See `docs/SPEED.md` for the full user-facing description.

pub mod agent;
pub mod date;
pub mod format;
pub mod menubar;
pub mod sampler;
pub mod store;

use std::sync::atomic::{AtomicBool, Ordering};

/// One snapshot of everything the menu bar and the menu display.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Display {
    pub download_bps: f64,
    pub upload_bps: f64,
    pub today_download_bytes: u64,
    pub today_upload_bytes: u64,
    pub window_download_bytes: u64,
    pub window_upload_bytes: u64,
}

/// Set when the meter should stop: the menu's *Turn off* item, SIGTERM,
/// SIGINT or SIGHUP.
static STOP: AtomicBool = AtomicBool::new(false);

/// Asks the agent to stop at the next opportunity.
pub fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
}

/// Whether a stop was requested.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}
