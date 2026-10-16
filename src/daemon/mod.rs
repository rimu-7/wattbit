//! The privileged daemon (`berry daemon run`, installed as `berryd`).
//!
//! Owns all hardware access. Serves JSON-RPC requests on a Unix socket,
//! re-evaluates the charge policy every [`EVALUATION_INTERVAL`] and on
//! sleep/wake, and restores factory charging behaviour on SIGTERM/SIGINT.

/// Writes a timestamped line to stderr (captured by launchd into the log).
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!(
            "{} berryd[{}]: {}",
            $crate::daemon::timestamp(),
            std::process::id(),
            format_args!($($arg)*)
        )
    };
}

mod controller;

use std::fs::{self, Permissions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{interval, timeout, MissedTickBehavior};

pub use controller::{Controller, HARDWARE_TIMEOUT};

use crate::capabilities::Support;
use crate::config::{Config, ConfigStore};
use crate::hardware::Hardware;
use crate::policy::EVALUATION_INTERVAL;
use crate::power;
use crate::protocol::{
    decode_request, encode_line, Call, ErrorKind, Response, RpcError, MAX_MESSAGE_BYTES,
    SERVER_READ_TIMEOUT, SERVER_WRITE_TIMEOUT,
};

/// Maximum number of client connections served concurrently.
const MAX_CONNECTIONS: usize = 16;

/// Time allowed for opening the SMC and probing capabilities at startup.
const STARTUP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub socket: PathBuf,
    pub config: PathBuf,
    /// Observe and report, but never write to the hardware.
    pub read_only: bool,
}

/// Credentials of a connected client, from `getpeereid(2)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

/// Decides whether `peer_uid` may make `call`.
///
/// Root, and the daemon's own uid (which already holds the daemon's
/// privileges), may do anything. Other users may only make read calls, and
/// only while non-root access is allowed.
pub fn authorize(
    call: &Call,
    peer_uid: libc::uid_t,
    daemon_uid: libc::uid_t,
    allow_non_root: bool,
) -> Result<(), RpcError> {
    if peer_uid == 0 || peer_uid == daemon_uid {
        return Ok(());
    }
    if call.requires_root() {
        return Err(RpcError::new(
            ErrorKind::Unauthorized,
            format!(
                "`{}` changes hardware state and requires root; re-run with sudo",
                call.method()
            ),
        ));
    }
    if !allow_non_root {
        return Err(RpcError::new(
            ErrorKind::Unauthorized,
            "non-root access to the daemon is disabled; re-run with sudo",
        ));
    }
    Ok(())
}

pub async fn run(options: DaemonOptions) -> anyhow::Result<()> {
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 && !options.read_only {
        bail!(
            "the daemon must run as root (pass --read-only to observe without changing hardware)"
        );
    }

    let store = ConfigStore::new(&options.config);
    let (config, config_warning) = match store.load() {
        Ok(Some(config)) => (config, None),
        Ok(None) => (Config::default(), None),
        Err(e) => {
            log!("{e}; using defaults until a setting is changed");
            (Config::default(), Some(format!("{e}; defaults are in use")))
        }
    };

    let verify_writes = euid == 0 && !options.read_only;
    let mut hardware = timeout(
        STARTUP_PROBE_TIMEOUT,
        tokio::task::spawn_blocking(move || Hardware::open(verify_writes)),
    )
    .await
    .context("probing hardware timed out")?
    .context("hardware probe task failed")?;

    let (power_tx, mut power_rx) = tokio::sync::mpsc::unbounded_channel();
    hardware.capabilities_mut().sleep_hooks = match power::spawn_sleep_watcher(power_tx) {
        Ok(features) => {
            if features.power_source_notifications {
                log!("power source notifications active: reacting to charge changes immediately");
            } else {
                log!(
                    "power source notifications unavailable; polling every {} s",
                    EVALUATION_INTERVAL.as_secs()
                );
            }
            Support::Supported
        }
        Err(e) => {
            log!("sleep hooks unavailable: {e}");
            Support::unsupported(e.to_string())
        }
    };
    log_capabilities(&hardware);

    let listener = bind_socket(&options.socket, config.allow_non_root_access)?;
    let controller = Arc::new(Controller::new(
        hardware,
        store,
        config,
        config_warning,
        options.socket.clone(),
        options.read_only,
    ));

    let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("installing SIGINT handler")?;
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut ticker = interval(EVALUATION_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    log!(
        "listening on {}{}",
        options.socket.display(),
        if options.read_only {
            " (read-only)"
        } else {
            ""
        }
    );

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => match Arc::clone(&connections).try_acquire_owned() {
                    Ok(permit) => {
                        tokio::spawn(serve(Arc::clone(&controller), stream, euid, permit));
                    }
                    Err(_) => log!("connection limit reached; dropping client"),
                },
                Err(e) => log!("accept failed: {e}"),
            },
            _ = ticker.tick() => controller.reconcile().await,
            Some(event) = power_rx.recv() => controller.handle_power_event(event).await,
            _ = sigterm.recv() => {
                log!("SIGTERM received, shutting down");
                break;
            }
            _ = sigint.recv() => {
                log!("SIGINT received, shutting down");
                break;
            }
        }
    }

    drop(listener);
    controller.shutdown().await;
    if let Err(e) = fs::remove_file(&options.socket) {
        if e.kind() != io::ErrorKind::NotFound {
            log!("cannot remove {}: {e}", options.socket.display());
        }
    }
    Ok(())
}

fn log_capabilities(hardware: &Hardware) {
    let caps = hardware.capabilities();
    let describe = |s: &Support| match s.reason() {
        None => "supported".to_owned(),
        Some(reason) if s.is_usable() => format!("unverified ({reason})"),
        Some(reason) => format!("unsupported ({reason})"),
    };
    log!(
        "{} {} macOS {}",
        caps.host.model.as_deref().unwrap_or("unknown model"),
        caps.host.arch,
        caps.host.os_version.as_deref().unwrap_or("unknown")
    );
    log!("charge inhibit: {}", describe(&caps.charge_inhibit));
    log!("firmware limit: {}", describe(&caps.firmware_limit));
    log!("adapter control: {}", describe(&caps.adapter_control));
    log!("MagSafe LED: {}", describe(&caps.magsafe_led));
    log!("sleep hooks: {}", describe(&caps.sleep_hooks));
}

/// Socket mode: world-connectable when non-root access is allowed (every
/// request is still authorized with `getpeereid`), owner-only otherwise.
pub fn socket_mode(allow_non_root: bool) -> u32 {
    if allow_non_root {
        0o666
    } else {
        0o600
    }
}

pub(crate) fn set_socket_permissions(path: &Path, allow_non_root: bool) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(socket_mode(allow_non_root)))
}

fn bind_socket(path: &Path, allow_non_root: bool) -> anyhow::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_socket() {
                bail!(
                    "{} exists and is not a socket; refusing to replace it",
                    path.display()
                );
            }
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                bail!("another daemon is already listening on {}", path.display());
            }
            fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", path.display())),
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    set_socket_permissions(path, allow_non_root)
        .with_context(|| format!("setting permissions on {}", path.display()))?;
    Ok(listener)
}

fn peer_credentials(stream: &UnixStream) -> io::Result<Peer> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the fd belongs to a live socket; uid/gid are valid out-pointers.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Peer { uid, gid })
}

async fn serve(
    controller: Arc<Controller>,
    stream: UnixStream,
    daemon_uid: libc::uid_t,
    _permit: OwnedSemaphorePermit,
) {
    let peer = match peer_credentials(&stream) {
        Ok(peer) => peer,
        Err(e) => {
            log!("getpeereid failed: {e}");
            return;
        }
    };
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).take(MAX_MESSAGE_BYTES as u64 + 1);
    let mut line = Vec::new();
    match timeout(SERVER_READ_TIMEOUT, reader.read_until(b'\n', &mut line)).await {
        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
        Ok(Ok(_)) => {}
    }

    let response = if line.len() > MAX_MESSAGE_BYTES {
        Response::error(
            None,
            RpcError::new(
                ErrorKind::InvalidRequest,
                format!("request exceeds {MAX_MESSAGE_BYTES} bytes"),
            ),
        )
    } else {
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        match decode_request(&line) {
            Err(response) => *response,
            Ok(request) => {
                let allow_non_root = controller.allow_non_root_access().await;
                let outcome = match authorize(&request.call, peer.uid, daemon_uid, allow_non_root) {
                    Ok(()) => controller.handle_call(request.call).await,
                    Err(e) => Err(e),
                };
                match outcome {
                    Ok(reply) => Response::result(request.id, reply),
                    Err(error) => Response::error(Some(request.id), error),
                }
            }
        }
    };

    let bytes = match encode_line(&response) {
        Ok(bytes) => bytes,
        Err(e) => {
            log!("cannot encode response: {e}");
            return;
        }
    };
    let write = async {
        write_half.write_all(&bytes).await?;
        write_half.shutdown().await
    };
    if let Ok(Err(e)) = timeout(SERVER_WRITE_TIMEOUT, write).await {
        log!("writing response to uid {} failed: {e}", peer.uid);
    }
}

/// Local time as `YYYY-MM-DD HH:MM:SS`.
pub(crate) fn timestamp() -> String {
    // SAFETY: time(NULL) has no preconditions.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: an all-zero `tm` is a valid value to be overwritten.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the duration of the call.
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return now.to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigOption;

    const DAEMON_ROOT: libc::uid_t = 0;

    #[test]
    fn root_may_do_anything() {
        let call = Call::SetAdapter { enabled: false };
        assert!(authorize(&call, 0, DAEMON_ROOT, false).is_ok());
    }

    #[test]
    fn users_may_read_only_when_allowed() {
        assert!(authorize(&Call::Status, 501, DAEMON_ROOT, true).is_ok());
        assert!(authorize(&Call::Doctor, 501, DAEMON_ROOT, true).is_ok());
        let denied = authorize(&Call::Status, 501, DAEMON_ROOT, false).unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Unauthorized);
    }

    #[test]
    fn users_may_never_write() {
        for call in [
            Call::SetLimit {
                upper: 80,
                lower: 75,
            },
            Call::DisableLimit,
            Call::SetAdapter { enabled: true },
            Call::SetOption {
                option: ConfigOption::AllowNonRoot,
                enabled: true,
            },
        ] {
            let err = authorize(&call, 501, DAEMON_ROOT, true).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Unauthorized, "{}", call.method());
        }
    }

    #[test]
    fn daemon_owner_has_daemon_privileges() {
        // An unprivileged development daemon accepts writes from its owner.
        assert!(authorize(&Call::DisableLimit, 501, 501, false).is_ok());
        assert!(authorize(&Call::DisableLimit, 502, 501, true).is_err());
    }

    #[test]
    fn socket_modes() {
        assert_eq!(socket_mode(true), 0o666);
        assert_eq!(socket_mode(false), 0o600);
    }
}
