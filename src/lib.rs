//! wattbit: battery charge limiting and an internet speed meter for macOS.
//!
//! Charge limiting is split into a privileged daemon (`wattbit daemon run`,
//! installed as `wattbitd`) that owns all SMC and IOKit interaction, and an
//! unprivileged CLI that talks to it over a Unix domain socket. The speed
//! meter (`wattbit speed`) is a separate per-user agent that needs no
//! privileges at all.

#[cfg(not(target_os = "macos"))]
compile_error!("wattbit only supports macOS");

pub mod battery;
pub mod capabilities;
pub mod cli;
pub mod client;
pub mod config;
pub mod conflicts;
pub mod daemon;
pub mod doctor;
pub mod ffi;
pub mod hardware;
pub mod legacy;
pub mod mcl;
pub mod policy;
pub mod power;
pub mod protocol;
pub mod service;
pub mod smc;
pub mod speed;
pub mod update;

/// Default location of the daemon's Unix domain socket.
pub const SOCKET_PATH: &str = "/var/run/wattbit.sock";

/// Default location of the persistent configuration file.
pub const CONFIG_PATH: &str = "/etc/wattbit.json";
