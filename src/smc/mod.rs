//! Low-level access to the System Management Controller through the
//! `AppleSMC` IOKit user client.
//!
//! The user client exposes a single struct method (selector 2) that takes
//! and returns an `SMCKeyData_t`. The sub-command is carried in `data8`.

pub mod keys;

use std::fmt::Write as _;
use std::mem::{offset_of, size_of};
use std::os::raw::c_void;

pub use keys::FourCC;

use crate::ffi;

/// Struct-method selector of the AppleSMC user client.
const KERNEL_INDEX_SMC: u32 = 2;
const SMC_CMD_READ_BYTES: u8 = 5;
const SMC_CMD_WRITE_BYTES: u8 = 6;
const SMC_CMD_READ_INDEX: u8 = 8;
const SMC_CMD_READ_KEYINFO: u8 = 9;

/// Key holding the total number of SMC keys, as a 4-byte big-endian count.
const KEY_COUNT: FourCC = FourCC::new(b"#KEY");

/// Size of the inline data buffer in `SMCKeyData_t`.
pub const SMC_MAX_DATA: usize = 32;

const SMC_RESULT_SUCCESS: u8 = 0x00;
const SMC_RESULT_KEY_NOT_FOUND: u8 = 0x84;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SmcVersion {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SmcPLimitData {
    version: u16,
    length: u16,
    cpu_p_limit: u32,
    gpu_p_limit: u32,
    mem_p_limit: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SmcKeyInfoData {
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
}

/// Mirror of `SMCKeyData_t` as consumed by the AppleSMC user client.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SmcKeyData {
    key: u32,
    vers: SmcVersion,
    p_limit_data: SmcPLimitData,
    key_info: SmcKeyInfoData,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; SMC_MAX_DATA],
}

// The kernel validates the struct size exactly; pin the C layout.
const _: () = {
    assert!(size_of::<SmcKeyData>() == 80);
    assert!(offset_of!(SmcKeyData, vers) == 4);
    assert!(offset_of!(SmcKeyData, p_limit_data) == 12);
    assert!(offset_of!(SmcKeyData, key_info) == 28);
    assert!(offset_of!(SmcKeyData, result) == 40);
    assert!(offset_of!(SmcKeyData, data8) == 42);
    assert!(offset_of!(SmcKeyData, data32) == 44);
    assert!(offset_of!(SmcKeyData, bytes) == 48);
};

/// Metadata the SMC reports for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInfo {
    pub size: u32,
    pub data_type: FourCC,
    pub attributes: u8,
}

/// A raw value read from the SMC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmcValue {
    pub key: FourCC,
    pub info: KeyInfo,
    pub bytes: Vec<u8>,
}

impl SmcValue {
    /// The raw bytes as space-separated hex, in the order the SMC returned them.
    pub fn hex(&self) -> String {
        hex(&self.bytes)
    }
}

/// Formats bytes as space-separated lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum SmcError {
    #[error("AppleSMC service not found in the IORegistry")]
    ServiceNotFound,
    #[error("IOServiceOpen(AppleSMC) failed: {}", io(.0))]
    Open(ffi::IOReturn),
    #[error("SMC key {0} does not exist on this machine")]
    KeyNotFound(FourCC),
    /// Returned for every write by a non-root caller, and for reads of the
    /// few keys the SMC protects (for example `CH0J` on macOS 27).
    #[error("access to this SMC key requires root privileges")]
    NotPrivileged,
    #[error("AppleSMC call for key {key} failed: {}", io(.code))]
    Call { key: FourCC, code: ffi::IOReturn },
    #[error("SMC rejected {op} of key {key} with result code {code:#04x}")]
    Rejected {
        key: FourCC,
        op: &'static str,
        code: u8,
    },
    #[error("SMC key {key} has size {actual}, expected {expected}")]
    SizeMismatch {
        key: FourCC,
        expected: usize,
        actual: usize,
    },
    #[error("SMC key {key} reports unsupported data size {size}")]
    InvalidSize { key: FourCC, size: u32 },
    #[error("SMC key {key} reads back [{actual}] after writing [{expected}]")]
    VerifyFailed {
        key: FourCC,
        expected: String,
        actual: String,
    },
    #[error("AppleSMC returned a truncated reply")]
    ShortReply,
}

fn io(code: &ffi::IOReturn) -> String {
    ffi::describe_ioreturn(*code)
}

fn check_result(key: FourCC, op: &'static str, code: u8) -> Result<(), SmcError> {
    match code {
        SMC_RESULT_SUCCESS => Ok(()),
        SMC_RESULT_KEY_NOT_FOUND => Err(SmcError::KeyNotFound(key)),
        code => Err(SmcError::Rejected { key, op, code }),
    }
}

/// An open connection to the AppleSMC user client.
///
/// Reads work for any user; writes require root. The connection is closed
/// on drop. Callers must serialize access (the daemon holds it behind a
/// mutex).
#[derive(Debug)]
pub struct Smc {
    conn: ffi::io_connect_t,
}

impl Smc {
    pub fn open() -> Result<Self, SmcError> {
        // SAFETY: the argument is a NUL-terminated C string literal. The
        // returned dictionary is retained or NULL.
        let matching = unsafe { ffi::IOServiceMatching(c"AppleSMC".as_ptr()) };
        if matching.is_null() {
            return Err(SmcError::ServiceNotFound);
        }
        // SAFETY: `matching` is a valid dictionary; this call consumes it.
        let service = unsafe {
            ffi::IOServiceGetMatchingService(ffi::IO_MAIN_PORT_DEFAULT, matching.cast_const())
        };
        if service == ffi::IO_OBJECT_NULL {
            return Err(SmcError::ServiceNotFound);
        }
        let mut conn: ffi::io_connect_t = ffi::IO_OBJECT_NULL;
        // SAFETY: `service` is a live service handle and `conn` is a valid
        // out-pointer. The service handle is released immediately after.
        let kr = unsafe {
            let kr = ffi::IOServiceOpen(service, mach2::traps::mach_task_self(), 0, &mut conn);
            ffi::IOObjectRelease(service);
            kr
        };
        if kr != ffi::kIOReturnSuccess {
            return Err(SmcError::Open(kr));
        }
        Ok(Self { conn })
    }

    fn call(&self, key: FourCC, input: &SmcKeyData) -> Result<SmcKeyData, SmcError> {
        let mut output = SmcKeyData::default();
        let mut output_size = size_of::<SmcKeyData>();
        // SAFETY: `input` and `output` are live repr(C) structs of exactly
        // the size passed alongside them.
        let kr = unsafe {
            ffi::IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                (input as *const SmcKeyData).cast::<c_void>(),
                size_of::<SmcKeyData>(),
                (&mut output as *mut SmcKeyData).cast::<c_void>(),
                &mut output_size,
            )
        };
        match kr {
            ffi::kIOReturnSuccess => {}
            ffi::kIOReturnNotPrivileged | ffi::kIOReturnNotPermitted => {
                return Err(SmcError::NotPrivileged)
            }
            code => return Err(SmcError::Call { key, code }),
        }
        if output_size < size_of::<SmcKeyData>() {
            return Err(SmcError::ShortReply);
        }
        Ok(output)
    }

    pub fn key_info(&self, key: FourCC) -> Result<KeyInfo, SmcError> {
        let input = SmcKeyData {
            key: key.to_u32(),
            data8: SMC_CMD_READ_KEYINFO,
            ..Default::default()
        };
        let output = self.call(key, &input)?;
        check_result(key, "key-info lookup", output.result)?;
        let size = output.key_info.data_size;
        if size == 0 || size as usize > SMC_MAX_DATA {
            return Err(SmcError::InvalidSize { key, size });
        }
        Ok(KeyInfo {
            size,
            data_type: FourCC::from_u32(output.key_info.data_type),
            attributes: output.key_info.data_attributes,
        })
    }

    pub fn read(&self, key: FourCC) -> Result<SmcValue, SmcError> {
        let info = self.key_info(key)?;
        let input = SmcKeyData {
            key: key.to_u32(),
            key_info: SmcKeyInfoData {
                data_size: info.size,
                ..Default::default()
            },
            data8: SMC_CMD_READ_BYTES,
            ..Default::default()
        };
        let output = self.call(key, &input)?;
        check_result(key, "read", output.result)?;
        Ok(SmcValue {
            key,
            info,
            bytes: output.bytes[..info.size as usize].to_vec(),
        })
    }

    /// Number of keys the SMC exposes.
    pub fn key_count(&self) -> Result<u32, SmcError> {
        let value = self.read(KEY_COUNT)?;
        let bytes: [u8; 4] = value
            .bytes
            .as_slice()
            .try_into()
            .map_err(|_| SmcError::SizeMismatch {
                key: KEY_COUNT,
                expected: 4,
                actual: value.bytes.len(),
            })?;
        Ok(u32::from_be_bytes(bytes))
    }

    /// Name of the key at `index` in the SMC's key table.
    pub fn key_at_index(&self, index: u32) -> Result<FourCC, SmcError> {
        let input = SmcKeyData {
            data8: SMC_CMD_READ_INDEX,
            data32: index,
            ..Default::default()
        };
        let output = self.call(KEY_COUNT, &input)?;
        check_result(KEY_COUNT, "index lookup", output.result)?;
        Ok(FourCC::from_u32(output.key))
    }

    /// Writes `data` to `key`. The length must match the key's size exactly.
    pub fn write(&self, key: FourCC, data: &[u8]) -> Result<(), SmcError> {
        let info = self.key_info(key)?;
        if data.len() != info.size as usize {
            return Err(SmcError::SizeMismatch {
                key,
                expected: info.size as usize,
                actual: data.len(),
            });
        }
        let mut input = SmcKeyData {
            key: key.to_u32(),
            key_info: SmcKeyInfoData {
                data_size: info.size,
                ..Default::default()
            },
            data8: SMC_CMD_WRITE_BYTES,
            ..Default::default()
        };
        input.bytes[..data.len()].copy_from_slice(data);
        let output = self.call(key, &input)?;
        check_result(key, "write", output.result)
    }

    /// Writes `data` to `key` and confirms the SMC reads it back unchanged.
    pub fn write_verified(&self, key: FourCC, data: &[u8]) -> Result<(), SmcError> {
        self.write(key, data)?;
        let readback = self.read(key)?;
        if readback.bytes != data {
            return Err(SmcError::VerifyFailed {
                key,
                expected: hex(data),
                actual: readback.hex(),
            });
        }
        Ok(())
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        // SAFETY: `conn` came from a successful IOServiceOpen and is closed
        // exactly once.
        unsafe {
            ffi::IOServiceClose(self.conn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_packs_big_endian() {
        let key: FourCC = "CH0C".parse().unwrap();
        assert_eq!(key.to_u32(), 0x4348_3043);
        assert_eq!(FourCC::from_u32(0x4348_3043), key);
        assert_eq!(FourCC::from_u32(u32::from_be_bytes(*b"ui8 ")).to_string(), "ui8 ");
    }

    #[test]
    fn fourcc_rejects_bad_input() {
        assert!("CH0".parse::<FourCC>().is_err());
        assert!("CH0CC".parse::<FourCC>().is_err());
        assert!("CH\u{7f}C".parse::<FourCC>().is_err());
    }

    #[test]
    fn result_codes_map_to_errors() {
        assert!(check_result(keys::CH0C, "read", 0).is_ok());
        assert!(matches!(
            check_result(keys::CH0C, "read", 0x84),
            Err(SmcError::KeyNotFound(_))
        ));
        assert!(matches!(
            check_result(keys::CH0C, "write", 0x86),
            Err(SmcError::Rejected { code: 0x86, .. })
        ));
    }

    #[test]
    fn binary_controls_are_consistent() {
        for control in keys::CHARGE_INHIBIT_CONTROLS
            .iter()
            .chain(keys::ADAPTER_CONTROLS)
        {
            assert_eq!(control.released.len(), control.size as usize);
            assert_eq!(control.engaged.len(), control.size as usize);
            assert_ne!(control.released, control.engaged);
            assert_eq!(control.classify(control.engaged), Some(true));
            assert_eq!(control.classify(control.released), Some(false));
        }
    }

    #[test]
    fn hex_formats_bytes() {
        assert_eq!(hex(&[0x01, 0x00, 0xff]), "01 00 ff");
        assert_eq!(hex(&[]), "");
    }
}
