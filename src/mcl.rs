//! macOS native Manual Charge Limit (MCL) via `PowerUI.framework`.
//!
//! On Apple Silicon Macs running macOS 15 (Sequoia) or later, Apple provides
//! native charge limiting through the private `PowerUI` subsystem. When active,
//! the firmware and PMIC keep the system powered directly from the AC charger
//! while completely inhibiting current flow to the battery cells (power bypass),
//! preventing battery wear without cycling.

use std::ffi::CStr;
use std::fmt;

type Id = *mut libc::c_void;
type Sel = *mut libc::c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const libc::c_char) -> Id;
    fn sel_registerName(name: *const libc::c_char) -> Sel;
    fn objc_msgSend();
}

#[derive(Debug, thiserror::Error)]
pub enum MclError {
    #[error("PowerUI framework is not available on this macOS system")]
    Unavailable,
    #[error("native charge limit is not supported on this hardware")]
    Unsupported,
    #[error("PowerUI client initialization failed")]
    InitFailed,
    #[error("setting charge limit to {0}% failed: {1}")]
    SetLimitFailed(u8, String),
    #[error("enabling native charge limit failed: {0}")]
    EnableFailed(String),
    #[error("disabling native charge limit failed: {0}")]
    DisableFailed(String),
    #[error("reading native charge limit failed: {0}")]
    ReadFailed(String),
}

/// Safe wrapper around `PowerUISmartChargeClient`.
pub struct Mcl {
    client: Id,
    available_limits: Vec<u8>,
}

// SAFETY: `PowerUISmartChargeClient` interacts with `PowerUIAgent` over XPC.
// The pointer is owned by `Mcl` and method calls are thread-safe or guarded.
unsafe impl Send for Mcl {}
// SAFETY: Concurrent immutable queries to PowerUISmartChargeClient dispatch safely.
unsafe impl Sync for Mcl {}

impl fmt::Debug for Mcl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mcl")
            .field("available_limits", &self.available_limits)
            .finish()
    }
}

impl Clone for Mcl {
    fn clone(&self) -> Self {
        if !self.client.is_null() {
            // SAFETY: self.client is a live Objective-C object responding to retain.
            unsafe {
                let sel_retain = sel_registerName(c"retain".as_ptr());
                let msg_ret: extern "C" fn(Id, Sel) -> Id =
                    std::mem::transmute(objc_msgSend as *const ());
                let _ = msg_ret(self.client, sel_retain);
            }
        }
        Self {
            client: self.client,
            available_limits: self.available_limits.clone(),
        }
    }
}

impl Mcl {
    /// Attempts to open the `PowerUI` client. Returns `None` if the framework
    /// or `PowerUISmartChargeClient` class is not present.
    pub fn open() -> Result<Self, MclError> {
        // SAFETY: dlopen loads the system private framework; Objective-C runtime functions
        // are invoked with exact signatures matching the Apple PowerUI framework headers.
        unsafe {
            let handle = libc::dlopen(
                c"/System/Library/PrivateFrameworks/PowerUI.framework/PowerUI".as_ptr(),
                libc::RTLD_LAZY,
            );
            if handle.is_null() {
                return Err(MclError::Unavailable);
            }

            let client_cls = objc_getClass(c"PowerUISmartChargeClient".as_ptr());
            if client_cls.is_null() {
                return Err(MclError::Unavailable);
            }

            let nsstring_cls = objc_getClass(c"NSString".as_ptr());
            let sel_string = sel_registerName(c"stringWithUTF8String:".as_ptr());
            let msg_str: extern "C" fn(Id, Sel, *const libc::c_char) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            let client_name = msg_str(nsstring_cls, sel_string, c"wattbit".as_ptr());

            let sel_alloc = sel_registerName(c"alloc".as_ptr());
            let msg_alloc: extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            let allocated = msg_alloc(client_cls, sel_alloc);

            let sel_init = sel_registerName(c"initWithClientName:".as_ptr());
            let msg_init: extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            let client = msg_init(allocated, sel_init, client_name);

            if client.is_null() {
                return Err(MclError::InitFailed);
            }

            let sel_is_supported = sel_registerName(c"isMCLSupported".as_ptr());
            let msg_supp: extern "C" fn(Id, Sel) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            if !msg_supp(client, sel_is_supported) {
                let sel_release = sel_registerName(c"release".as_ptr());
                let msg_rel: extern "C" fn(Id, Sel) =
                    std::mem::transmute(objc_msgSend as *const ());
                msg_rel(client, sel_release);
                return Err(MclError::Unsupported);
            }

            let available_limits = Self::read_available_limits(client);

            Ok(Self {
                client,
                available_limits,
            })
        }
    }

    // SAFETY: client must be a valid PowerUISmartChargeClient instance.
    unsafe fn read_available_limits(client: Id) -> Vec<u8> {
        // SAFETY: ObjC calls match the PowerUISmartChargeClient selector signatures.
        unsafe {
            let sel_limits = sel_registerName(c"availableChargeLimitsWithError:".as_ptr());
            let mut err: Id = std::ptr::null_mut();
            let msg_limits: extern "C" fn(Id, Sel, *mut Id) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            let arr = msg_limits(client, sel_limits, &mut err);

            if arr.is_null() {
                // Default to standard macOS limits if array read returns null
                return vec![80, 85, 90, 95, 100];
            }

            let sel_count = sel_registerName(c"count".as_ptr());
            let msg_count: extern "C" fn(Id, Sel) -> usize =
                std::mem::transmute(objc_msgSend as *const ());
            let count = msg_count(arr, sel_count);

            let sel_object_at_index = sel_registerName(c"objectAtIndex:".as_ptr());
            let msg_object: extern "C" fn(Id, Sel, usize) -> Id =
                std::mem::transmute(objc_msgSend as *const ());

            let sel_unsigned_char = sel_registerName(c"unsignedCharValue".as_ptr());
            let msg_uchar: extern "C" fn(Id, Sel) -> u8 =
                std::mem::transmute(objc_msgSend as *const ());

            let mut limits = Vec::with_capacity(count);
            for i in 0..count {
                let item = msg_object(arr, sel_object_at_index, i);
                if !item.is_null() {
                    limits.push(msg_uchar(item, sel_unsigned_char));
                }
            }
            if limits.is_empty() {
                vec![80, 85, 90, 95, 100]
            } else {
                limits
            }
        }
    }

    pub fn available_limits(&self) -> &[u8] {
        &self.available_limits
    }

    /// Finds the closest supported limit for a requested target percentage.
    pub fn snap_limit(&self, target: u8) -> u8 {
        if self.available_limits.contains(&target) {
            return target;
        }
        if let Some(&closest) = self
            .available_limits
            .iter()
            .min_by_key(|&&lim| (lim as i16 - target as i16).abs())
        {
            closest
        } else {
            80
        }
    }

    /// Checks if native charge limit (MCL) is currently enabled.
    pub fn is_enabled(&self) -> Result<bool, MclError> {
        // SAFETY: self.client is a live PowerUISmartChargeClient instance.
        unsafe {
            let sel = sel_registerName(c"isMCLCurrentlyEnabled:".as_ptr());
            let mut err: Id = std::ptr::null_mut();
            let msg: extern "C" fn(Id, Sel, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let enabled = msg(self.client, sel, &mut err);
            if !err.is_null() {
                let desc = Self::format_error(err);
                return Err(MclError::ReadFailed(desc));
            }
            Ok(enabled)
        }
    }

    /// Reads the currently configured manual charge limit percentage.
    pub fn get_limit(&self) -> Result<u8, MclError> {
        // SAFETY: self.client is a live PowerUISmartChargeClient instance.
        unsafe {
            let sel = sel_registerName(c"getMCLLimitWithError:".as_ptr());
            let mut err: Id = std::ptr::null_mut();
            let msg: extern "C" fn(Id, Sel, *mut Id) -> u8 =
                std::mem::transmute(objc_msgSend as *const ());
            let limit = msg(self.client, sel, &mut err);
            if !err.is_null() {
                let desc = Self::format_error(err);
                return Err(MclError::ReadFailed(desc));
            }
            Ok(limit)
        }
    }

    /// Sets the manual charge limit percentage and enables MCL.
    pub fn set_limit(&self, percent: u8) -> Result<(), MclError> {
        let snapped = self.snap_limit(percent);
        // SAFETY: self.client is a live PowerUISmartChargeClient instance; selectors match declared signatures.
        unsafe {
            let sel_set = sel_registerName(c"setMCLLimit:error:".as_ptr());
            let mut err: Id = std::ptr::null_mut();
            let msg_set: extern "C" fn(Id, Sel, u8, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let success = msg_set(self.client, sel_set, snapped, &mut err);
            if !success || !err.is_null() {
                let desc = Self::format_error(err);
                return Err(MclError::SetLimitFailed(snapped, desc));
            }

            let sel_enable = sel_registerName(c"enableMCL:".as_ptr());
            let mut enable_err: Id = std::ptr::null_mut();
            let msg_enable: extern "C" fn(Id, Sel, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let en_success = msg_enable(self.client, sel_enable, &mut enable_err);
            if !en_success || !enable_err.is_null() {
                let desc = Self::format_error(enable_err);
                return Err(MclError::EnableFailed(desc));
            }

            Ok(())
        }
    }

    /// Disables manual charge limit, returning to factory 100% charging.
    pub fn disable(&self) -> Result<(), MclError> {
        // SAFETY: self.client is a live PowerUISmartChargeClient instance; selectors match declared signatures.
        unsafe {
            let sel_disable = sel_registerName(c"disableMCL:".as_ptr());
            let mut err: Id = std::ptr::null_mut();
            let msg_disable: extern "C" fn(Id, Sel, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let success = msg_disable(self.client, sel_disable, &mut err);
            if !success || !err.is_null() {
                let desc = Self::format_error(err);
                return Err(MclError::DisableFailed(desc));
            }

            // Also reset limit to 100
            let sel_set = sel_registerName(c"setMCLLimit:error:".as_ptr());
            let mut set_err: Id = std::ptr::null_mut();
            let msg_set: extern "C" fn(Id, Sel, u8, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let _ = msg_set(self.client, sel_set, 100, &mut set_err);

            Ok(())
        }
    }

    // SAFETY: err must be a valid NSError object or null.
    unsafe fn format_error(err: Id) -> String {
        // SAFETY: Objective-C calls to NSError description and NSString UTF8String match their signatures.
        unsafe {
            if err.is_null() {
                return "unknown error".into();
            }
            let sel_desc = sel_registerName(c"description".as_ptr());
            let sel_utf8 = sel_registerName(c"UTF8String".as_ptr());
            let msg_obj: extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            let desc = msg_obj(err, sel_desc);
            let msg_cstr: extern "C" fn(Id, Sel) -> *const libc::c_char =
                std::mem::transmute(objc_msgSend as *const ());
            let utf8 = msg_cstr(desc, sel_utf8);
            if utf8.is_null() {
                "failed to read error description".into()
            } else {
                CStr::from_ptr(utf8).to_string_lossy().into_owned()
            }
        }
    }
}

impl Drop for Mcl {
    fn drop(&mut self) {
        if !self.client.is_null() {
            // SAFETY: self.client was retained or created by this instance and responds to release.
            unsafe {
                let sel_release = sel_registerName(c"release".as_ptr());
                let msg_rel: extern "C" fn(Id, Sel) =
                    std::mem::transmute(objc_msgSend as *const ());
                msg_rel(self.client, sel_release);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_limit_selects_closest() {
        let mcl = Mcl {
            client: std::ptr::null_mut(),
            available_limits: vec![80, 85, 90, 95, 100],
        };
        assert_eq!(mcl.snap_limit(80), 80);
        assert_eq!(mcl.snap_limit(85), 85);
        assert_eq!(mcl.snap_limit(75), 80);
        assert_eq!(mcl.snap_limit(50), 80);
        assert_eq!(mcl.snap_limit(82), 80);
        assert_eq!(mcl.snap_limit(83), 85);
        assert_eq!(mcl.snap_limit(99), 100);
    }
}
