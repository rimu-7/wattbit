//! berry: battery charge-limit management for macOS.
//!
//! The crate is split into a privileged daemon (`berry daemon run`, installed
//! as `berryd`) that owns all SMC and IOKit interaction, and an unprivileged
//! CLI that talks to it over a Unix domain socket.

#[cfg(not(target_os = "macos"))]
compile_error!("berry only supports macOS");

pub mod battery;
pub mod capabilities;
pub mod config;
pub mod ffi;
pub mod hardware;
pub mod policy;
pub mod power;
pub mod protocol;
pub mod smc;

/// Default location of the daemon's Unix domain socket.
pub const SOCKET_PATH: &str = "/var/run/berry.sock";

/// Default location of the persistent configuration file.
pub const CONFIG_PATH: &str = "/etc/berry.json";
