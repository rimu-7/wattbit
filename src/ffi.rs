//! Raw declarations for the IOKit APIs berry calls.
//!
//! Every signature and constant here is transcribed from the macOS SDK
//! headers (`IOKit/IOKitLib.h`, `IOKit/IOReturn.h`, `IOKit/IOMessage.h`,
//! `IOKit/pwr_mgt/IOPMLib.h` and `IOKit/ps/IOPowerSources.h`). Only the
//! symbols berry actually uses are declared.

#![allow(non_camel_case_types, non_upper_case_globals)]

use std::os::raw::{c_char, c_void};

use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::{CFAllocatorRef, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation_sys::runloop::CFRunLoopSourceRef;
use core_foundation_sys::string::CFStringRef;
pub use mach2::kern_return::{kern_return_t, KERN_SUCCESS};
pub use mach2::port::{mach_port_t, MACH_PORT_NULL};

pub type io_object_t = mach_port_t;
pub type io_service_t = io_object_t;
pub type io_connect_t = io_object_t;
pub type io_registry_entry_t = io_object_t;
pub type task_port_t = mach_port_t;
pub type IOReturn = kern_return_t;
pub type IOOptionBits = u32;
pub type IOPMAssertionID = u32;
pub type IOPMAssertionLevel = u32;
pub type IONotificationPortRef = *mut c_void;

/// `IOServiceInterestCallback` from `IOKitLib.h`.
pub type IOServiceInterestCallback = unsafe extern "C" fn(
    refcon: *mut c_void,
    service: io_service_t,
    message_type: u32,
    message_argument: *mut c_void,
);

/// `IOPowerSourceCallbackType` from `IOPowerSources.h`.
pub type IOPowerSourceCallbackType = unsafe extern "C" fn(context: *mut c_void);

/// `IO_OBJECT_NULL`.
pub const IO_OBJECT_NULL: io_object_t = 0;

/// `kIOMainPortDefault` is an `extern const` whose documented value is
/// `NULL` ("the NULL argument indicates use the default").
pub const IO_MAIN_PORT_DEFAULT: mach_port_t = MACH_PORT_NULL;

// IOReturn.h: iokit_common_err(x) = sys_iokit | sub_iokit_common | x, where
// sys_iokit = err_system(0x38) = 0xe0000000 and sub_iokit_common = 0.
const fn iokit_common_err(code: u32) -> IOReturn {
    (0xe000_0000_u32 | code) as IOReturn
}

pub const kIOReturnSuccess: IOReturn = KERN_SUCCESS;
pub const kIOReturnNoDevice: IOReturn = iokit_common_err(0x2c0);
pub const kIOReturnNotPrivileged: IOReturn = iokit_common_err(0x2c1);
pub const kIOReturnBadArgument: IOReturn = iokit_common_err(0x2c2);
pub const kIOReturnUnsupported: IOReturn = iokit_common_err(0x2c7);
pub const kIOReturnTimeout: IOReturn = iokit_common_err(0x2d6);
pub const kIOReturnNotPermitted: IOReturn = iokit_common_err(0x2e2);

// IOMessage.h: iokit_common_msg(x) = sys_iokit | sub_iokit_common | x.
pub const kIOMessageCanSystemSleep: u32 = 0xe000_0270;
pub const kIOMessageSystemWillSleep: u32 = 0xe000_0280;
pub const kIOMessageSystemWillNotSleep: u32 = 0xe000_0290;
pub const kIOMessageSystemHasPoweredOn: u32 = 0xe000_0300;

/// `kIOPMAssertionLevelOn` from `IOPMLib.h`.
pub const kIOPMAssertionLevelOn: IOPMAssertionLevel = 255;

/// Value of `kIOPMAssertPreventUserIdleSystemSleep`.
pub const ASSERTION_PREVENT_USER_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";
/// Value of `kIOPMAssertionTypePreventSystemSleep`.
pub const ASSERTION_PREVENT_SYSTEM_SLEEP: &str = "PreventSystemSleep";

// IOPSKeys.h
pub const kIOPSTypeKey: &str = "Type";
pub const kIOPSInternalBatteryType: &str = "InternalBattery";
pub const kIOPSIsPresentKey: &str = "Is Present";
pub const kIOPSPowerSourceStateKey: &str = "Power Source State";
pub const kIOPSACPowerValue: &str = "AC Power";
pub const kIOPSBatteryPowerValue: &str = "Battery Power";
pub const kIOPSCurrentCapacityKey: &str = "Current Capacity";
pub const kIOPSMaxCapacityKey: &str = "Max Capacity";
pub const kIOPSIsChargingKey: &str = "Is Charging";
pub const kIOPSIsChargedKey: &str = "Is Charged";
pub const kIOPSTimeToEmptyKey: &str = "Time to Empty";
pub const kIOPSTimeToFullChargeKey: &str = "Time to Full Charge";
pub const kIOPSVoltageKey: &str = "Voltage";
pub const kIOPSCurrentKey: &str = "Current";
pub const kIOPSBatteryHealthKey: &str = "BatteryHealth";

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOKitLib.h
    pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    /// Consumes one reference to `matching`.
    pub fn IOServiceGetMatchingService(
        main_port: mach_port_t,
        matching: CFDictionaryRef,
    ) -> io_service_t;
    pub fn IOServiceOpen(
        service: io_service_t,
        owning_task: task_port_t,
        type_: u32,
        connect: *mut io_connect_t,
    ) -> kern_return_t;
    pub fn IOServiceClose(connect: io_connect_t) -> kern_return_t;
    pub fn IOObjectRelease(object: io_object_t) -> kern_return_t;
    pub fn IOConnectCallStructMethod(
        connection: mach_port_t,
        selector: u32,
        input_struct: *const c_void,
        input_struct_cnt: usize,
        output_struct: *mut c_void,
        output_struct_cnt: *mut usize,
    ) -> kern_return_t;
    pub fn IORegistryEntryCreateCFProperties(
        entry: io_registry_entry_t,
        properties: *mut CFMutableDictionaryRef,
        allocator: CFAllocatorRef,
        options: IOOptionBits,
    ) -> kern_return_t;
    pub fn IONotificationPortGetRunLoopSource(notify: IONotificationPortRef) -> CFRunLoopSourceRef;

    // IOPMLib.h
    pub fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut io_object_t,
    ) -> io_connect_t;
    pub fn IOAllowPowerChange(kernel_port: io_connect_t, notification_id: isize) -> IOReturn;
    pub fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        assertion_level: IOPMAssertionLevel,
        assertion_name: CFStringRef,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;
    pub fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;

    // IOPowerSources.h
    pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    pub fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
    pub fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFDictionaryRef;
    /// Returns an owned run loop source (Create rule), or NULL.
    pub fn IOPSNotificationCreateRunLoopSource(
        callback: IOPowerSourceCallbackType,
        context: *mut c_void,
    ) -> CFRunLoopSourceRef;
}

/// Human-readable rendering of an `IOReturn`, naming the codes berry
/// expects to encounter.
pub fn describe_ioreturn(code: IOReturn) -> String {
    let name = match code {
        kIOReturnSuccess => "kIOReturnSuccess",
        kIOReturnNoDevice => "kIOReturnNoDevice",
        kIOReturnNotPrivileged => "kIOReturnNotPrivileged",
        kIOReturnBadArgument => "kIOReturnBadArgument",
        kIOReturnUnsupported => "kIOReturnUnsupported",
        kIOReturnTimeout => "kIOReturnTimeout",
        kIOReturnNotPermitted => "kIOReturnNotPermitted",
        _ => return format!("{:#010x}", code as u32),
    };
    format!("{name} ({:#010x})", code as u32)
}
