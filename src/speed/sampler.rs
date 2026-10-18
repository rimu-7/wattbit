//! Network interface counters.
//!
//! macOS publishes 64-bit per-interface byte counters through the routing
//! sysctl `NET_RT_IFLIST2`. The buffer is a sequence of routing messages;
//! each `RTM_IFINFO2` message is an `if_msghdr2` (with 64-bit `if_data64`)
//! followed by a `sockaddr_dl` naming the interface — the same layout
//! `netstat -b` reads.
//!
//! Only real link-layer interfaces are counted. Loopback, Wireless Direct
//! Link and tunnel/bridge interfaces are skipped because their traffic
//! already crosses (or bypasses) a physical interface, and counting them
//! would double the totals.

use std::io;
use std::ptr;

/// `IFT_ETHER` from `<net/if.h>`; libc does not export it.
const IFT_ETHER: u8 = 0x06;

/// Interface-name prefixes whose traffic is not counted. The physical
/// interfaces that carry that traffic are counted instead.
pub const EXCLUDED_PREFIXES: &[&str] = &[
    "lo",     // loopback
    "awdl",   // Apple Wireless Direct Link
    "llw",    // low-latency WLAN
    "utun",   // tunnels (VPN, Back to My Mac)
    "gif",    // generic tunnel
    "stf",    // 6to4
    "ap",     // hotspot
    "anpi",   // internal accessory interfaces
    "bridge", // Thunderbolt/Internet-Sharing bridges
    "vmenet", // VM guest interfaces
    "vmnet",  // VM host interfaces
    "p2p",    // Wi-Fi peer-to-peer
];

/// Byte counters of one interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceCounters {
    pub name: String,
    /// `ifi_type` from the kernel.
    pub kind: u8,
    pub received: u64,
    pub sent: u64,
}

/// Summed byte counters since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub received: u64,
    pub sent: u64,
}

impl Counters {
    /// `self - previous`, saturating at zero. A smaller current value means
    /// the interface counters were reset (reboot, driver reload), which is
    /// reported as no traffic rather than as a bogus spike.
    pub fn delta_since(self, previous: Self) -> Self {
        Self {
            received: self.received.saturating_sub(previous.received),
            sent: self.sent.saturating_sub(previous.sent),
        }
    }

    pub fn total(self) -> u64 {
        self.received.saturating_add(self.sent)
    }
}

/// Whether an interface's traffic is counted.
pub fn counted(interface: &InterfaceCounters) -> bool {
    interface.kind == IFT_ETHER
        && !EXCLUDED_PREFIXES
            .iter()
            .any(|prefix| interface.name.starts_with(prefix))
}

/// Reads every interface's counters.
pub fn read() -> io::Result<Vec<InterfaceCounters>> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    // The interface list can grow between the sizing call and the read;
    // retry instead of failing.
    for _ in 0..4 {
        let mut size: usize = 0;
        // SAFETY: a null buffer asks sysctl for the required size.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                ptr::null_mut(),
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buffer = vec![0u8; size];
        // SAFETY: `buffer` is a writable region of `size` bytes.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buffer.as_mut_ptr().cast(),
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOMEM) {
                continue;
            }
            return Err(error);
        }
        buffer.truncate(size);
        return Ok(parse(buffer.as_slice()));
    }
    Err(io::Error::other(
        "the interface list kept growing between reads",
    ))
}

/// Sums the counters of every counted interface.
pub fn totals() -> io::Result<Counters> {
    Ok(read()?.iter().filter(|interface| counted(interface)).fold(
        Counters::default(),
        |acc, interface| Counters {
            received: acc.received.saturating_add(interface.received),
            sent: acc.sent.saturating_add(interface.sent),
        },
    ))
}

/// Parses an `NET_RT_IFLIST2` buffer into per-interface counters.
pub fn parse(buffer: &[u8]) -> Vec<InterfaceCounters> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= buffer.len() {
        // SAFETY: bounds are checked above, and the pointer is not aligned
        // (messages are packed), so the length is read unaligned.
        let length = u16::from_ne_bytes([buffer[offset], buffer[offset + 1]]) as usize;
        if length < 4 || offset + length > buffer.len() {
            break;
        }
        if let Some(interface) = parse_message(&buffer[offset..offset + length]) {
            found.push(interface);
        }
        offset += length;
    }
    found
}

fn parse_message(message: &[u8]) -> Option<InterfaceCounters> {
    if message.len() < std::mem::size_of::<libc::if_msghdr2>() {
        return None;
    }
    // SAFETY: `if_msghdr2` is `repr(packed(4))` and bigger than four bytes;
    // `read_unaligned` copies it without assuming alignment.
    let header: libc::if_msghdr2 =
        unsafe { ptr::read_unaligned(message.as_ptr().cast::<libc::if_msghdr2>()) };
    if i32::from(header.ifm_type) != libc::RTM_IFINFO2 {
        return None;
    }

    // The interface name lives in the `sockaddr_dl` that follows the header.
    let name_at = std::mem::size_of::<libc::if_msghdr2>();
    let address = message.get(name_at..)?;
    if address.len() < std::mem::size_of::<libc::sockaddr_dl>() {
        return None;
    }
    // SAFETY: `sockaddr_dl` is byte-aligned (`c_uchar` fields first) and the
    // slice is long enough, checked above.
    let link: libc::sockaddr_dl =
        unsafe { ptr::read_unaligned(address.as_ptr().cast::<libc::sockaddr_dl>()) };
    let name_length = usize::from(link.sdl_nlen);
    let name_bytes = address.get(8..8 + name_length)?;
    let name = std::str::from_utf8(name_bytes).ok()?.to_owned();

    Some(InterfaceCounters {
        name,
        kind: header.ifm_data.ifi_type,
        received: header.ifm_data.ifi_ibytes,
        sent: header.ifm_data.ifi_obytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a message exactly as the kernel lays it out, so the parser is
    /// tested against the real binary format.
    fn message(name: &str, kind: u8, received: u64, sent: u64) -> Vec<u8> {
        let mut bytes = vec![0u8; std::mem::size_of::<libc::if_msghdr2>()];
        let header = libc::if_msghdr2 {
            ifm_msglen: 0,
            ifm_version: 2,
            ifm_type: libc::RTM_IFINFO2 as u8,
            ifm_addrs: 0,
            ifm_flags: 0,
            ifm_index: 0,
            ifm_snd_len: 0,
            ifm_snd_maxlen: 0,
            ifm_snd_drops: 0,
            ifm_timer: 0,
            ifm_data: libc::if_data64 {
                ifi_type: kind,
                ifi_typelen: 0,
                ifi_physical: 0,
                ifi_addrlen: 0,
                ifi_hdrlen: 0,
                ifi_recvquota: 0,
                ifi_xmitquota: 0,
                ifi_unused1: 0,
                ifi_mtu: 1500,
                ifi_metric: 0,
                ifi_baudrate: 0,
                ifi_ipackets: 0,
                ifi_ierrors: 0,
                ifi_opackets: 0,
                ifi_oerrors: 0,
                ifi_collisions: 0,
                ifi_ibytes: received,
                ifi_obytes: sent,
                ifi_imcasts: 0,
                ifi_omcasts: 0,
                ifi_iqdrops: 0,
                ifi_noproto: 0,
                ifi_recvtiming: 0,
                ifi_xmittiming: 0,
                ifi_lastchange: libc::timeval32 {
                    tv_sec: 0,
                    tv_usec: 0,
                },
            },
        };
        // SAFETY: writing a (packed) struct into a buffer at least as large.
        unsafe {
            ptr::write_unaligned(bytes.as_mut_ptr().cast::<libc::if_msghdr2>(), header);
        }
        // sockaddr_dl: length, family, index, type, nlen, alen, slen, name.
        let name_bytes = name.as_bytes();
        let mut link = vec![0u8; 8];
        link[0] = (8 + name_bytes.len()) as u8;
        link[1] = libc::AF_LINK as u8;
        link[4] = IFT_ETHER; // sdl_type
        link[5] = name_bytes.len() as u8;
        link.extend_from_slice(name_bytes);
        link.resize(std::mem::size_of::<libc::sockaddr_dl>(), 0);
        bytes.extend_from_slice(&link);
        let length = bytes.len() as u16;
        bytes[..2].copy_from_slice(&length.to_ne_bytes());
        bytes
    }

    #[test]
    fn parses_the_kernel_message_layout() {
        let buffer = message("en0", IFT_ETHER, 1_000, 2_000);
        let parsed = parse(&buffer);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0],
            InterfaceCounters {
                name: "en0".into(),
                kind: IFT_ETHER,
                received: 1_000,
                sent: 2_000,
            }
        );
    }

    #[test]
    fn walks_multiple_messages_and_ignores_other_types() {
        let mut buffer = message("en0", IFT_ETHER, 10, 20);
        buffer.extend_from_slice(&message("utun3", 0, 1, 2));
        buffer.extend_from_slice(&message("en1", IFT_ETHER, 30, 40));
        let parsed = parse(&buffer);
        let names: Vec<&str> = parsed.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["en0", "utun3", "en1"]);
    }

    #[test]
    fn truncated_and_garbage_buffers_are_safe() {
        assert!(parse(&[]).is_empty());
        assert!(parse(&[0]).is_empty());
        assert!(parse(&[0xff, 0xff, 0, 0]).is_empty());
        let mut message = message("en0", IFT_ETHER, 1, 2);
        message.truncate(message.len() - 4);
        // The message header alone still parses the name if bytes remain...
        let parsed = parse(&message);
        assert!(parsed.len() <= 1);
        // ...but a zero length stops the walk instead of looping.
        let bad = vec![0u8; 64];
        assert!(parse(&bad).is_empty());
    }

    #[test]
    fn filtering_skips_loopback_tunnels_and_virtual_interfaces() {
        let interface = |name: &str, kind: u8| InterfaceCounters {
            name: name.into(),
            kind,
            received: 0,
            sent: 0,
        };
        assert!(counted(&interface("en0", IFT_ETHER)));
        assert!(counted(&interface("en7", IFT_ETHER)));
        assert!(!counted(&interface("lo0", IFT_ETHER)));
        assert!(!counted(&interface("awdl0", IFT_ETHER)));
        assert!(!counted(&interface("utun0", IFT_ETHER)));
        assert!(!counted(&interface("bridge0", IFT_ETHER)));
        assert!(!counted(&interface("gif0", IFT_ETHER)));
        // utun-type interfaces are not Ethernet, so they are skipped even
        // if their name is unexpected.
        assert!(!counted(&interface("weird0", 0)));
    }

    #[test]
    fn delta_saturates_on_counter_resets() {
        let previous = Counters {
            received: 1_000,
            sent: 500,
        };
        let current = Counters {
            received: 1_500,
            sent: 900,
        };
        assert_eq!(
            current.delta_since(previous),
            Counters {
                received: 500,
                sent: 400
            }
        );
        let reset = Counters {
            received: 10,
            sent: 5,
        };
        assert_eq!(reset.delta_since(previous), Counters::default());
        assert_eq!(Counters::default().total(), 0);
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore)]
    fn reading_this_machine_reports_physical_interfaces() {
        let interfaces = read().expect("the routing sysctl must be readable");
        assert!(
            interfaces.iter().any(|i| i.name.starts_with("en")),
            "expected at least one en* interface: {interfaces:?}"
        );
        assert!(!interfaces.iter().any(|i| i.name == "lo0" && counted(i)));
        let totals = totals().expect("totals must be readable");
        assert!(totals.total() > 0, "a booted Mac has moved some bytes");
    }
}
