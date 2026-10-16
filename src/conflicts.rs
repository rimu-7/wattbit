//! Detection of other charge-limit tools.
//!
//! batt, AlDente, actuallymentor's `battery` and Battery Toolkit write the
//! same SMC keys as berry. Two controllers driving one key make the charging
//! state oscillate, so the daemon pauses its hardware writes while another
//! one is running and reports it instead of fighting.
//!
//! Processes are identified by executable path and arguments
//! (`KERN_PROCARGS2`), falling back to `proc_pidpath` when arguments are not
//! readable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::ptr;

use serde::{Deserialize, Serialize};

/// Arguments examined per process.
const MAX_ARGS: usize = 16;

/// Longest command line kept in a report.
const MAX_DETAIL: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub tool: String,
    /// The running process, or `None` for an installed but idle tool.
    pub pid: Option<u32>,
    /// Command line of the process, or the installed path.
    pub detail: String,
    /// Shell commands that stop and remove the tool.
    pub remedy: String,
}

/// A running process as seen by the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub executable: String,
    pub args: Vec<String>,
}

impl ProcessInfo {
    fn strings(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.executable.as_str()).chain(self.args.iter().map(String::as_str))
    }

    fn command_line(&self) -> String {
        let line = if self.args.is_empty() {
            self.executable.clone()
        } else {
            self.args.join(" ")
        };
        let mut cut = line.len().min(MAX_DETAIL);
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line[..cut].to_owned()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KnownTool {
    pub name: &'static str,
    pub remedy: &'static str,
    /// Paths whose presence means the tool is installed. `~/` is the
    /// invoking user's home directory.
    pub installed: &'static [&'static str],
    matches: fn(&ProcessInfo) -> bool,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_batt(p: &ProcessInfo) -> bool {
    basename(&p.executable) == "batt" || p.args.first().is_some_and(|a| basename(a) == "batt")
}

fn is_battery_cli(p: &ProcessInfo) -> bool {
    p.strings()
        .any(|s| s.contains("co.palokaj.battery") || s == "/usr/local/bin/battery")
}

fn is_aldente(p: &ProcessInfo) -> bool {
    p.executable.to_ascii_lowercase().contains("aldente")
}

fn is_battery_toolkit(p: &ProcessInfo) -> bool {
    let exe = p.executable.to_ascii_lowercase();
    exe.contains("batterytoolkit") || exe.contains("battery toolkit")
}

pub const KNOWN_TOOLS: &[KnownTool] = &[
    KnownTool {
        name: "batt",
        remedy: "sudo launchctl bootout system/cc.chlc.batt; \
                 sudo rm -f /Library/LaunchDaemons/cc.chlc.batt.plist /usr/local/bin/batt",
        installed: &["/Library/LaunchDaemons/cc.chlc.batt.plist"],
        matches: is_batt,
    },
    KnownTool {
        name: "battery (actuallymentor)",
        remedy: "launchctl bootout gui/$(id -u)/com.battery.app; pkill -f co.palokaj.battery; \
                 rm -f ~/Library/LaunchAgents/battery.plist",
        installed: &[
            "~/Library/LaunchAgents/battery.plist",
            "/usr/local/co.palokaj.battery",
        ],
        matches: is_battery_cli,
    },
    KnownTool {
        name: "AlDente",
        remedy: "quit AlDente and remove it with its own uninstaller, then: \
                 sudo launchctl bootout system/com.apphousekitchen.aldente-pro.helper; \
                 sudo rm -f /Library/LaunchDaemons/com.apphousekitchen.aldente-pro.helper.plist",
        installed: &[
            "/Library/LaunchDaemons/com.apphousekitchen.aldente-pro.helper.plist",
            "/Applications/AlDente.app",
            "/Applications/AlDente Pro.app",
        ],
        matches: is_aldente,
    },
    KnownTool {
        name: "Battery Toolkit",
        remedy: "quit Battery Toolkit and move it from /Applications to the Trash",
        installed: &["/Applications/Battery Toolkit.app"],
        matches: is_battery_toolkit,
    },
];

/// The known tool `process` belongs to, if any.
pub fn classify(process: &ProcessInfo) -> Option<&'static KnownTool> {
    KNOWN_TOOLS.iter().find(|tool| (tool.matches)(process))
}

/// Charge limiters currently running, one entry per process.
pub fn running() -> Vec<Conflict> {
    let own = std::process::id();
    let mut buffer = vec![0u8; argmax()];
    let mut found = Vec::new();
    for pid in all_pids() {
        if pid <= 0 || pid as u32 == own {
            continue;
        }
        let Some(process) = process_info(pid, &mut buffer) else {
            continue;
        };
        if let Some(tool) = classify(&process) {
            found.push(Conflict {
                tool: tool.name.to_owned(),
                pid: Some(process.pid),
                detail: process.command_line(),
                remedy: tool.remedy.to_owned(),
            });
        }
    }
    found
}

/// Charge limiters installed on disk but not in `running`.
pub fn installed(running: &[Conflict]) -> Vec<Conflict> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut found = Vec::new();
    for tool in KNOWN_TOOLS {
        if running.iter().any(|c| c.tool == tool.name) {
            continue;
        }
        let present = tool.installed.iter().find_map(|path| {
            let full = match path.strip_prefix("~/") {
                Some(rest) => home.as_ref()?.join(rest),
                None => PathBuf::from(path),
            };
            full.exists().then_some(full)
        });
        if let Some(path) = present {
            found.push(Conflict {
                tool: tool.name.to_owned(),
                pid: None,
                detail: path.display().to_string(),
                remedy: tool.remedy.to_owned(),
            });
        }
    }
    found
}

/// Picks `one` or `many` by the number of distinct tools, so messages
/// agree with [`summarize`]: "batt controls", "batt, AlDente control".
pub fn agree<'a>(conflicts: &[Conflict], one: &'a str, many: &'a str) -> &'a str {
    let mut tools: Vec<&str> = conflicts.iter().map(|c| c.tool.as_str()).collect();
    tools.sort_unstable();
    tools.dedup();
    if tools.len() == 1 {
        one
    } else {
        many
    }
}

/// One-line summary grouping processes by tool, e.g.
/// `batt (pid 7125), battery (actuallymentor) (pids 11877, 11904)`.
pub fn summarize(conflicts: &[Conflict]) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut pids: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for c in conflicts {
        if !order.contains(&c.tool.as_str()) {
            order.push(&c.tool);
        }
        pids.entry(&c.tool).or_default().extend(c.pid);
    }
    order
        .iter()
        .map(|tool| match pids[tool].as_slice() {
            [] => (*tool).to_owned(),
            [pid] => format!("{tool} (pid {pid})"),
            many => {
                let list: Vec<String> = many.iter().map(u32::to_string).collect();
                format!("{tool} (pids {})", list.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn all_pids() -> Vec<libc::pid_t> {
    // SAFETY: a null buffer asks for the number of processes.
    let count = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    // Leave room for processes started between the two calls.
    let mut pids = vec![0 as libc::pid_t; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: `pids` is a writable buffer of `bytes` bytes.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(count.max(0) as usize);
    pids
}

fn argmax() -> usize {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `value`/`len` describe a valid buffer.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.argmax".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && value > 0 {
        (value as usize).clamp(4096, 4 << 20)
    } else {
        256 * 1024
    }
}

fn process_info(pid: libc::pid_t, buffer: &mut [u8]) -> Option<ProcessInfo> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = buffer.len();
    // SAFETY: `mib` has three entries and `buffer`/`size` describe a valid
    // writable region.
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
    if rc == 0 {
        if let Some((executable, args)) = parse_procargs(&buffer[..size.min(buffer.len())]) {
            return Some(ProcessInfo {
                pid: pid as u32,
                executable,
                args,
            });
        }
    }
    // Arguments of other users' processes are unreadable without root, and
    // the path fails for a process whose binary was deleted while running.
    // The short BSD info (process name) is readable for every process.
    executable_path(pid)
        .or_else(|| process_name(pid))
        .map(|executable| ProcessInfo {
            pid: pid as u32,
            executable,
            args: Vec::new(),
        })
}

fn process_name(pid: libc::pid_t) -> Option<String> {
    // SAFETY: an all-zero proc_bsdshortinfo is a valid value to overwrite.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    let bytes: Vec<u8> = info
        .pbsi_comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn executable_path(pid: libc::pid_t) -> Option<String> {
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `path` is a writable buffer of the given size.
    let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&path[..len as usize]).into_owned())
}

/// Parses a `KERN_PROCARGS2` buffer: a native-endian `int argc`, the
/// executable path, NUL padding, then `argc` NUL-terminated arguments.
pub fn parse_procargs(data: &[u8]) -> Option<(String, Vec<String>)> {
    let argc = i32::from_ne_bytes(data.get(..4)?.try_into().ok()?);
    if argc < 0 {
        return None;
    }
    let rest = &data[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let executable = String::from_utf8_lossy(&rest[..exec_end]).into_owned();
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    let mut args = Vec::new();
    while args.len() < (argc as usize).min(MAX_ARGS) && pos < rest.len() {
        let end = rest[pos..]
            .iter()
            .position(|&b| b == 0)
            .map_or(rest.len(), |e| pos + e);
        args.push(String::from_utf8_lossy(&rest[pos..end]).into_owned());
        pos = end + 1;
    }
    Some((executable, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(executable: &str, args: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid: 42,
            executable: executable.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn tool(p: &ProcessInfo) -> Option<&'static str> {
        classify(p).map(|t| t.name)
    }

    #[test]
    fn recognizes_known_tools() {
        let batt = process(
            "/usr/local/bin/batt",
            &["/usr/local/bin/batt", "daemon", "--log-level=debug"],
        );
        assert_eq!(tool(&batt), Some("batt"));

        let battery = process(
            "/bin/bash",
            &[
                "/bin/bash",
                "/usr/local/co.palokaj.battery/battery",
                "maintain_synchronous",
                "80",
            ],
        );
        assert_eq!(tool(&battery), Some("battery (actuallymentor)"));

        let aldente = process(
            "/Library/PrivilegedHelperTools/com.apphousekitchen.aldente-pro.helper",
            &[],
        );
        assert_eq!(tool(&aldente), Some("AlDente"));

        let toolkit = process(
            "/Applications/Battery Toolkit.app/Contents/MacOS/Battery Toolkit",
            &[],
        );
        assert_eq!(tool(&toolkit), Some("Battery Toolkit"));
    }

    #[test]
    fn ignores_unrelated_processes() {
        for p in [
            process(
                "/System/Library/ExtensionKit/Extensions/BatterySettingsIntentsExtension.appex/Contents/MacOS/BatterySettingsIntentsExtension",
                &[],
            ),
            process("/Library/PrivilegedHelperTools/berryd", &["berryd", "daemon", "run"]),
            process("/usr/bin/grep", &["grep", "battery"]),
            process("/opt/homebrew/bin/battery-monitor", &[]),
        ] {
            assert_eq!(tool(&p), None, "{p:?}");
        }
    }

    #[test]
    fn parses_procargs_layout() {
        let mut data = 3i32.to_ne_bytes().to_vec();
        data.extend_from_slice(
            b"/bin/bash\0\0\0\0/bin/bash\0/usr/local/bin/battery\0maintain\0PATH=/usr/bin\0",
        );
        let (exe, args) = parse_procargs(&data).unwrap();
        assert_eq!(exe, "/bin/bash");
        assert_eq!(args, ["/bin/bash", "/usr/local/bin/battery", "maintain"]);
    }

    #[test]
    fn rejects_malformed_procargs() {
        assert_eq!(parse_procargs(&[1, 0]), None);
        assert_eq!(parse_procargs(&(-1i32).to_ne_bytes()), None);
        let mut no_nul = 1i32.to_ne_bytes().to_vec();
        no_nul.extend_from_slice(b"/bin/sh");
        assert_eq!(parse_procargs(&no_nul), None);
    }

    #[test]
    fn summary_groups_processes() {
        let c = |tool: &str, pid| Conflict {
            tool: tool.into(),
            pid,
            detail: String::new(),
            remedy: String::new(),
        };
        let summary = summarize(&[
            c("batt", Some(7)),
            c("battery", Some(8)),
            c("battery", Some(9)),
        ]);
        assert_eq!(summary, "batt (pid 7), battery (pids 8, 9)");
        assert_eq!(summarize(&[c("AlDente", None)]), "AlDente");
    }

    #[test]
    fn agreement_follows_distinct_tools() {
        let c = |tool: &str, pid| Conflict {
            tool: tool.into(),
            pid: Some(pid),
            detail: String::new(),
            remedy: String::new(),
        };
        // Two processes of one tool read as singular.
        let one = [c("battery", 1), c("battery", 2)];
        assert_eq!(agree(&one, "controls", "control"), "controls");
        let two = [c("battery", 1), c("batt", 2)];
        assert_eq!(agree(&two, "controls", "control"), "control");
    }

    #[test]
    fn scanning_this_machine_does_not_flag_itself() {
        let own = std::process::id();
        assert!(running().iter().all(|c| c.pid != Some(own)));
    }
}
