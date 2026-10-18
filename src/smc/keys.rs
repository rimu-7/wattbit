//! SMC key identifiers and the exact byte patterns wattbit writes to them.
//!
//! wattbit never interprets multi-byte control values numerically. Each
//! control is described by the literal bytes for its two states, which keeps
//! writes independent of host and SMC byte order.

use std::fmt;
use std::str::FromStr;

/// A four-character code, as used for SMC key names and data types.
///
/// AppleSMC packs a FourCC into a `u32` with the first character in the most
/// significant byte, independent of host byte order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FourCC([u8; 4]);

impl FourCC {
    pub const fn new(code: &[u8; 4]) -> Self {
        Self(*code)
    }

    pub const fn from_u32(value: u32) -> Self {
        Self(value.to_be_bytes())
    }

    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    pub const fn bytes(self) -> [u8; 4] {
        self.0
    }
}

impl fmt::Display for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in &self.0 {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCC(\"{self}\")")
    }
}

/// Error returned when parsing a string that is not a valid SMC key name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("SMC keys are exactly four printable ASCII characters, got {0:?}")]
pub struct InvalidFourCC(pub String);

impl FromStr for FourCC {
    type Err = InvalidFourCC;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes: [u8; 4] = s
            .as_bytes()
            .try_into()
            .map_err(|_| InvalidFourCC(s.to_owned()))?;
        if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
            Ok(Self(bytes))
        } else {
            Err(InvalidFourCC(s.to_owned()))
        }
    }
}

/// Charge inhibit on newer Apple Silicon firmware (macOS 26 era).
pub const CHTE: FourCC = FourCC::new(b"CHTE");
/// Charge inhibit on earlier Apple Silicon and T2 firmware.
pub const CH0B: FourCC = FourCC::new(b"CH0B");
/// Charge inhibit on earlier Apple Silicon and T2 firmware.
pub const CH0C: FourCC = FourCC::new(b"CH0C");
/// Adapter (input power) disable on newer Apple Silicon firmware.
pub const CHIE: FourCC = FourCC::new(b"CHIE");
/// Adapter (input power) disable on earlier firmware.
pub const CH0I: FourCC = FourCC::new(b"CH0I");
/// Battery Charge Level Max: firmware-enforced charge ceiling on Intel Macs.
pub const BCLM: FourCC = FourCC::new(b"BCLM");
/// MagSafe connector LED control.
pub const ACLC: FourCC = FourCC::new(b"ACLC");
/// AC input indicator; probed for diagnostics only, never written.
pub const ACIN: FourCC = FourCC::new(b"ACIN");
/// Protected charge key on macOS 27 firmware; probed for diagnostics only,
/// never written.
pub const CH0J: FourCC = FourCC::new(b"CH0J");
/// Protected charge key on macOS 27 firmware; probed for diagnostics only,
/// never written.
pub const CHLS: FourCC = FourCC::new(b"CHLS");

/// A two-state hardware control implemented by writing fixed byte patterns
/// to one or more SMC keys. All keys of a control are always written
/// together.
#[derive(Debug)]
pub struct BinaryControl {
    pub name: &'static str,
    pub keys: &'static [FourCC],
    /// Expected data size of every key, in bytes.
    pub size: u32,
    /// Bytes for the factory state: charging allowed, or adapter in use.
    pub released: &'static [u8],
    /// Bytes for the engaged state: charging inhibited, or adapter bypassed.
    pub engaged: &'static [u8],
}

impl BinaryControl {
    /// Classifies a raw value: `Some(true)` if engaged, `Some(false)` if
    /// released, `None` if the value is neither known pattern.
    pub fn classify(&self, bytes: &[u8]) -> Option<bool> {
        if bytes == self.engaged {
            Some(true)
        } else if bytes == self.released {
            Some(false)
        } else {
            None
        }
    }

    pub fn pattern(&self, engaged: bool) -> &'static [u8] {
        if engaged {
            self.engaged
        } else {
            self.released
        }
    }
}

pub const CHARGE_INHIBIT_CHTE: BinaryControl = BinaryControl {
    name: "CHTE",
    keys: &[CHTE],
    size: 4,
    released: &[0x00, 0x00, 0x00, 0x00],
    engaged: &[0x01, 0x00, 0x00, 0x00],
};

pub const CHARGE_INHIBIT_CH0B_CH0C: BinaryControl = BinaryControl {
    name: "CH0B/CH0C",
    keys: &[CH0B, CH0C],
    size: 1,
    released: &[0x00],
    engaged: &[0x02],
};

pub const ADAPTER_CHIE: BinaryControl = BinaryControl {
    name: "CHIE",
    keys: &[CHIE],
    size: 1,
    released: &[0x00],
    engaged: &[0x08],
};

pub const ADAPTER_CH0I: BinaryControl = BinaryControl {
    name: "CH0I",
    keys: &[CH0I],
    size: 1,
    released: &[0x00],
    engaged: &[0x01],
};

/// Charge-inhibit implementations in order of preference.
pub const CHARGE_INHIBIT_CONTROLS: &[&BinaryControl] =
    &[&CHARGE_INHIBIT_CHTE, &CHARGE_INHIBIT_CH0B_CH0C];

/// Adapter-disable implementations in order of preference.
pub const ADAPTER_CONTROLS: &[&BinaryControl] = &[&ADAPTER_CHIE, &ADAPTER_CH0I];

/// Highest value accepted by `BCLM`; 100 means "no limit".
pub const BCLM_MAX: u8 = 100;

/// MagSafe LED states written to `ACLC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedMode {
    /// Hand LED control back to the system.
    System,
    Off,
    Green,
    Orange,
}

impl LedMode {
    pub const fn byte(self) -> u8 {
        match self {
            LedMode::System => 0x00,
            LedMode::Off => 0x01,
            LedMode::Green => 0x03,
            LedMode::Orange => 0x04,
        }
    }
}

/// A key wattbit probes, with a short description for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct KeyDescriptor {
    pub key: FourCC,
    pub purpose: &'static str,
}

/// Every key wattbit knows about, in the order diagnostics list them.
pub const KNOWN_KEYS: &[KeyDescriptor] = &[
    KeyDescriptor {
        key: CHTE,
        purpose: "charge inhibit (newer Apple Silicon firmware)",
    },
    KeyDescriptor {
        key: CH0B,
        purpose: "charge inhibit (earlier firmware)",
    },
    KeyDescriptor {
        key: CH0C,
        purpose: "charge inhibit (earlier firmware)",
    },
    KeyDescriptor {
        key: CHIE,
        purpose: "adapter disable (newer Apple Silicon firmware)",
    },
    KeyDescriptor {
        key: CH0I,
        purpose: "adapter disable (earlier firmware)",
    },
    KeyDescriptor {
        key: BCLM,
        purpose: "firmware charge ceiling (Intel)",
    },
    KeyDescriptor {
        key: ACLC,
        purpose: "MagSafe LED",
    },
    KeyDescriptor {
        key: ACIN,
        purpose: "AC input indicator (diagnostic only)",
    },
    KeyDescriptor {
        key: CH0J,
        purpose: "charge control (macOS 27 firmware, diagnostic only)",
    },
    KeyDescriptor {
        key: CHLS,
        purpose: "charge limit state (macOS 27 firmware, diagnostic only)",
    },
];
