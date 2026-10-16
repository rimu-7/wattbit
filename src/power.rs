//! System power integration: sleep/wake notifications and power assertions.

use std::io;
use std::os::raw::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::ffi;

/// How long the sleep callback waits for the daemon to prepare before
/// acknowledging. The system itself gives up after 30 seconds.
pub const SLEEP_ACK_TIMEOUT: Duration = Duration::from_secs(10);

const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionKind {
    PreventUserIdleSystemSleep,
    PreventSystemSleep,
}

impl AssertionKind {
    fn type_name(self) -> &'static str {
        match self {
            AssertionKind::PreventUserIdleSystemSleep => {
                ffi::ASSERTION_PREVENT_USER_IDLE_SYSTEM_SLEEP
            }
            AssertionKind::PreventSystemSleep => ffi::ASSERTION_PREVENT_SYSTEM_SLEEP,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PowerError {
    #[error("IOPMAssertionCreateWithName failed: {}", ffi::describe_ioreturn(*.0))]
    Assertion(ffi::IOReturn),
    #[error("IORegisterForSystemPower failed")]
    Registration,
    #[error("cannot start power notification thread: {0}")]
    Thread(#[from] io::Error),
    #[error("power notification thread did not start in time")]
    Timeout,
}

/// A held IOPM assertion, released on drop.
#[derive(Debug)]
pub struct PowerAssertion {
    id: ffi::IOPMAssertionID,
    kind: AssertionKind,
}

impl PowerAssertion {
    pub fn acquire(kind: AssertionKind, reason: &str) -> Result<Self, PowerError> {
        let type_name = CFString::from_static_string(kind.type_name());
        let name = CFString::new(reason);
        let mut id: ffi::IOPMAssertionID = 0;
        // SAFETY: both strings are live CFStrings and `id` is a valid
        // out-pointer.
        let kr = unsafe {
            ffi::IOPMAssertionCreateWithName(
                type_name.as_concrete_TypeRef(),
                ffi::kIOPMAssertionLevelOn,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if kr != ffi::kIOReturnSuccess {
            return Err(PowerError::Assertion(kr));
        }
        Ok(Self { id, kind })
    }

    pub fn kind(&self) -> AssertionKind {
        self.kind
    }
}

impl Drop for PowerAssertion {
    fn drop(&mut self) {
        // SAFETY: `id` came from a successful create and is released once.
        unsafe {
            ffi::IOPMAssertionRelease(self.id);
        }
    }
}

/// System power transitions delivered to the daemon.
#[derive(Debug)]
pub enum PowerEvent {
    /// The system is about to sleep. Send on `ack` once prepared; sleep
    /// proceeds after the acknowledgement or [`SLEEP_ACK_TIMEOUT`].
    WillSleep { ack: mpsc::SyncSender<()> },
    /// The system finished waking.
    HasPoweredOn,
}

struct WatcherContext {
    root_port: AtomicU32,
    events: UnboundedSender<PowerEvent>,
}

unsafe extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: ffi::io_service_t,
    message_type: u32,
    message_argument: *mut c_void,
) {
    // SAFETY: `refcon` is the leaked `WatcherContext` registered below,
    // which lives for the rest of the process.
    let context = unsafe { &*(refcon as *const WatcherContext) };
    let notification_id = message_argument as isize;
    let acknowledge = || {
        // SAFETY: the root port was stored before the run loop started.
        unsafe {
            ffi::IOAllowPowerChange(context.root_port.load(Ordering::Acquire), notification_id);
        }
    };
    match message_type {
        // berry never vetoes idle sleep.
        ffi::kIOMessageCanSystemSleep => acknowledge(),
        ffi::kIOMessageSystemWillSleep => {
            let (ack, done) = mpsc::sync_channel(1);
            if context.events.send(PowerEvent::WillSleep { ack }).is_ok() {
                let _ = done.recv_timeout(SLEEP_ACK_TIMEOUT);
            }
            acknowledge();
        }
        ffi::kIOMessageSystemHasPoweredOn => {
            let _ = context.events.send(PowerEvent::HasPoweredOn);
        }
        _ => {}
    }
}

/// Registers for system sleep/wake notifications on a dedicated thread
/// running a CFRunLoop. The registration lasts for the life of the process.
pub fn spawn_sleep_watcher(events: UnboundedSender<PowerEvent>) -> Result<(), PowerError> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), PowerError>>();
    thread::Builder::new()
        .name("berry-power".into())
        .spawn(move || {
            let context = Box::into_raw(Box::new(WatcherContext {
                root_port: AtomicU32::new(0),
                events,
            }));
            let mut port: ffi::IONotificationPortRef = ptr::null_mut();
            let mut notifier: ffi::io_object_t = ffi::IO_OBJECT_NULL;
            // SAFETY: `context` stays valid for the process lifetime and the
            // out-pointers are valid.
            let root = unsafe {
                ffi::IORegisterForSystemPower(
                    context.cast(),
                    &mut port,
                    power_callback,
                    &mut notifier,
                )
            };
            if root == ffi::IO_OBJECT_NULL || port.is_null() {
                // SAFETY: registration failed, so nothing else references it.
                drop(unsafe { Box::from_raw(context) });
                let _ = ready_tx.send(Err(PowerError::Registration));
                return;
            }
            // SAFETY: `context` is valid (leaked above).
            unsafe { (*context).root_port.store(root, Ordering::Release) };
            // SAFETY: `port` is a live notification port; its run loop source
            // is added to this thread's run loop, which then runs forever.
            unsafe {
                let source = ffi::IONotificationPortGetRunLoopSource(port);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            }
            let _ = ready_tx.send(Ok(()));
            // SAFETY: runs this thread's run loop until process exit.
            unsafe { CFRunLoopRun() };
        })?;
    ready_rx
        .recv_timeout(REGISTRATION_TIMEOUT)
        .map_err(|_| PowerError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertions_can_be_taken_and_released() {
        let assertion =
            PowerAssertion::acquire(AssertionKind::PreventUserIdleSystemSleep, "berry unit test")
                .expect("unprivileged processes may hold idle-sleep assertions");
        assert_eq!(assertion.kind(), AssertionKind::PreventUserIdleSystemSleep);
    }
}
