# The wattbit speed meter

`wattbit speed` shows your live internet speeds in the MacBook menu bar and
keeps a per-day history of how much you have downloaded and uploaded over
the last 31 days. Records older than that window are deleted automatically.

It is the second half of wattbit, next to charge limiting, and it is fully
independent of it: the speed meter needs no `sudo`, never touches the SMC,
never talks to the `wattbitd` daemon and works even when charge control is
unsupported on your Mac.

- [Quick start](#quick-start)
- [Commands](#commands)
- [The menu bar item](#the-menu-bar-item)
- [Reading `wattbit speed status`](#reading-wattbit-speed-status)
- [Usage history and retention](#usage-history-and-retention)
- [What is counted](#what-is-counted)
- [How it works](#how-it-works)
- [Troubleshooting](#troubleshooting)
- [Removing the meter](#removing-the-meter)
- [For developers](#for-developers)

## Quick start

The [installer](../README.md#install) turns the meter on for you:

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
```

After that, from any Terminal:

```sh
# Start the meter (if you installed with --no-speed or turned it off).
wattbit speed on

# Look at today's and the month's totals at any time.
wattbit speed status

# Stop showing and recording.
wattbit speed off
```

`wattbit speed on` installs a login item (`~/Library/LaunchAgents/com.wattbit.speed.plist`),
so the meter is already there after your next login. It runs as *you*, in
your login session — never as root. Do **not** use `sudo` with the `speed`
commands; if you do, wattbit refuses and tells you to re-run it without sudo.

## Commands

| Command | What it does | Needs `sudo` |
| --- | --- | --- |
| `wattbit speed on` | Shows the meter in the menu bar and starts recording | no |
| `wattbit speed off` | Hides the meter, stops recording; the history is kept | no |
| `wattbit speed status` | Shows live speeds and the last 31 days of usage | no |
| `wattbit speed status --json` | The same report as JSON, for scripts | no |

All three are safe to run twice: `on` notices a running meter instead of
starting a second one, and `off` on a stopped meter just says so.

`wattbit speed on --no-launchd` starts the meter without installing the login
item. It stops when you log out; use it to try things out.

The meter and the charge-limit commands don't interact. You can run either,
both or neither.

## The menu bar item

While the meter is on, an item like this sits in the right-hand side of the
menu bar, just left of the system icons:

```text
↓ 1.23 MB/s ↑ 340 KB/s
```

- `↓` is download (traffic received), `↑` is upload (traffic sent).
- The figures are live rates, refreshed once per second.
- Click the item to see the current rates spelled out, today's totals, the
  totals over the last 31 days, and **Turn off speed meter**, which stops the
  meter exactly like `wattbit speed off`.
- When a newer wattbit release is out, the menu opens with an update
  section:

  ```text
  ⚠️ Update available: wattbit 0.4.0 (you have 0.3.0)
  Run this in Terminal to update:
      curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
  Copy Update Command
  ```

  **Copy Update Command** puts the command on your clipboard; paste it into
  Terminal and press Return. The update keeps your usage history and
  restarts the meter on the new version. See
  [Updating](../README.md#updating) for how the check
  works and how to turn it off.

macOS decides the exact slot; wattbit can't programmatically place itself
"after the Wi-Fi icon". To move it, hold ⌘ and drag the item where you want
it — anywhere in the menu bar, including to the right of the system icons.
macOS remembers the position (wattbit sets a status-item autosave name), so it
comes back to the same place after restarts and logins.

If there is no logged-in graphical session (for example over SSH), the item
can't exist. The meter still records; `wattbit speed status` shows the live
figures as long as the agent answers its socket.

## Reading `wattbit speed status`

```text
Internet speed meter: on (pid 4321, since 2026-10-18 15:55:38)
  Live speed: ↓ 1.23 MB/s  ↑ 340 KB/s

Internet usage, last 31 days (2026-09-18 to 2026-10-18):
  Today:  ↓ 24.3 MB  ↑ 8.96 MB
  Total:  ↓ 214 MB  ↑ 54.2 MB
  Average: ↓ 30.6 MB  ↑ 7.74 MB  per recorded day (7 of 31)

  Date          Download      Upload       Total
  2026-10-18     24.3 MB     8.96 MB     33.3 MB
  2026-10-17     41.0 MB     12.4 MB     53.5 MB
  ...

Records older than this window are deleted automatically.
```

- **Internet speed meter** says whether the agent is running, and since when.
- **Live speed** comes from the running agent over a private Unix socket. If
  the agent is starting or busy it says `unavailable`; the history below is
  still shown from disk.
- **Today**, **Total** and the table count the retention window only. The
  table lists each recorded day, newest first; days with no traffic aren't
  listed.
- **Average** divides by the days that actually have a record, not by 31.
- While the meter is off, the history just sits on disk and the last line
  changes to "The meter is off, so no usage is being recorded."

`--json` prints the same report for scripts:

```json
{
  "running": true,
  "pid": 4321,
  "started_at": 1792317252,
  "live": {
    "pid": 4321,
    "started_at": 1792317252,
    "sample_at": 1792317258,
    "interval_seconds": 1.0,
    "download_bps": 3068.39,
    "upload_bps": 1022.79,
    "today_download_bytes": 47104,
    "today_upload_bytes": 280576,
    "window_download_bytes": 47104,
    "window_upload_bytes": 280576
  },
  "usage": {
    "window_start": "2026-09-18",
    "window_end": "2026-10-18",
    "retention_days": 31,
    "days": [{ "date": "2026-10-18", "download_bytes": 1024, "upload_bytes": 3072 }],
    "recorded_days": 1,
    "today": { "date": "2026-10-18", "download_bytes": 1024, "upload_bytes": 3072 },
    "total_download_bytes": 1024,
    "total_upload_bytes": 3072,
    "average_download_bytes": 1024,
    "average_upload_bytes": 3072
  },
  "warning": null,
  "update": null
}
```

`live` is `null` when the agent isn't answering; `usage` is `null` before
anything has been recorded; `warning` explains a usage file that couldn't be
read; `update` is `null` unless a newer release is known, and then holds
`current`, `latest` and the `command` that installs it. Byte counts are plain integers; the `/s` figures are bytes per second.

## Usage history and retention

Everything lives in one private directory:

| Path | What it is |
| --- | --- |
| `~/Library/Application Support/wattbit/speed.json` | The usage history |
| `~/Library/Application Support/wattbit/speed.pid` | The running agent's pid, while it runs |
| `~/Library/Application Support/wattbit/speed.sock` | Live-status socket, while it runs |
| `~/Library/Application Support/wattbit/speed.log` | The agent's log |
| `~/Library/Application Support/wattbit/update.json` | The latest wattbit release and when it was checked |
| `~/Library/LaunchAgents/com.wattbit.speed.plist` | The login item, while the meter is on |

The directory is created with mode `0700` and the files with `0600`: only
your account can read them.

**Retention.** The history keeps today and the previous 30 days, by local
calendar date — 31 days in total. Older records are deleted from disk
whenever the meter is running (at startup and on every sampling tick) and
again when you run `wattbit speed off`, so the window holds even if you leave
the meter off for months. `wattbit speed status` never shows anything outside
the window.

**The file.** `speed.json` is small JSON, versioned so future wattbit releases
can migrate it:

```json
{
  "version": 1,
  "days": [
    { "date": "2026-10-18", "download_bytes": 24334336, "upload_bytes": 8960000 }
  ]
}
```

It is replaced atomically (written to a temporary file, synced, then
renamed), so a crash or a power cut can't leave a half-written history. If
the file is somehow unreadable, the agent moves it aside as
`speed.json.corrupt.<time>` and starts a fresh history; `wattbit speed status`
reports the problem in the `warning` field rather than failing.

**Saving.** The agent flushes accumulated traffic to disk every 30 seconds
and on clean exit (signal or **Turn off speed meter**). A hard kill can lose
up to 30 seconds of counting; nothing else.

## What is counted

Live rates and history come from the kernel's own 64-bit per-interface byte
counters (`sysctl` `NET_RT_IFLIST2` — the same source as `netstat -b`), so
wattbit sees exactly what the hardware moved, with no packet capture and no
extra drivers.

- Only real link-layer interfaces are counted: Ethernet, Wi-Fi, USB
  tethering, and so on.
- Loopback (`lo0`) is ignored, and so are interfaces whose traffic already
  crosses a counted one — `awdl`, `llw`, `utun` (VPNs), `gif`, `stf`, `ap`
  (the hotspot interface), `anpi`, `bridge`, `vmenet`, `vmnet` and `p2p`,
  so nothing is counted twice. VPN traffic appears on the underlying
  Wi-Fi/Ethernet interface; hotspot clients' traffic appears once, on the
  internet uplink they are sharing.
- Counters reset when an interface is reset (reboot, driver reload, Wi-Fi
  off/on). wattbit treats a decreasing counter as zero rather than inventing a
  huge spike.
- Traffic while the meter is off is never recorded: the first sample after
  `wattbit speed on` only establishes the baseline.
- The rates are the byte delta between two samples divided by the actual
  elapsed time. If a sample gap is longer than 30 seconds (the Mac woke from
  sleep), the bytes are still added to the totals but no rate is shown for
  that gap — otherwise it would read as a spike.
- A day is attributed by the moment a sample lands. With the normal
  one-second interval the error is at most one second of traffic around
  midnight.

Units are decimal SI, the networking convention: 1 KB is 1000 bytes, 1 MB is
1000 KB. So `↓ 1.23 MB/s ↑ 340 KB/s` is exactly what a speed test would call
1.23 MB/s down and 340 KB/s up.

## How it works

```text
wattbit speed on
   ├─ writes ~/Library/LaunchAgents/com.wattbit.speed.plist
   ├─ asks launchd (your GUI session) to start it
   └─ waits for the agent to answer via its pid file
        │
        ▼
   wattbit speed agent            (one process per user, runs as you)
   ├─ main thread: AppKit menu bar item, updates once per second
   ├─ sampler thread: kernels counters → today's usage → speed.json
   └─ Unix socket: answers `wattbit speed status` with live figures
```

- The **login item** is a per-user LaunchAgent, not a system daemon. It
  carries `RunAtLoad` (start at login) and `KeepAlive` (restart after a
  crash, but not after a clean exit), and is limited to the Aqua session,
  so it only ever runs inside your desktop login. `wattbit speed off` removes
  the plist and unloads the job; while it stays loaded, `launchctl` would
  restart a killed agent, which is why `off` always unloads first. The
  menu's **Turn off speed meter** exits cleanly, which launchd leaves
  stopped.
- If launchd can't start the agent (no GUI session, unusual setup), `on`
  falls back to starting it directly, detached from the terminal with
  `setsid`, logging to `speed.log`. `--no-launchd` requests that path
  explicitly.
- The **agent's main thread** owns the menu bar, because AppKit must be
  driven from the main thread. It pumps the run loop and repaints the title
  when a new sample arrives.
- The **sampler** is a small Tokio runtime on a worker thread: it ticks every
  second (adjustable), reads the counters, accumulates into the in-memory
  history, answers socket queries, and exits cleanly on
  SIGTERM/SIGINT/SIGHUP or the menu's *Turn off* item. There is one stop
  path everywhere: `wattbit speed off` sends SIGTERM; the menu item sets the
  same stop flag in-process.
- Only the sampling thread touches the network counters, and only the main
  thread touches AppKit, so no locks are shared between them.
- The menu bar, like the rest of wattbit's hardware access, is built on
  hand-written Objective-C runtime declarations — no new dependencies. It
  uses `NSStatusBar`/`NSStatusItem`, and the item's autosave name is
  `com.wattbit.speed`.
- Three environment variables exist for tests and unusual setups:
  `WATTBIT_SPEED_DIR` moves the data directory, `WATTBIT_SPEED_NO_GUI=1`
  records without trying the menu bar, and `WATTBIT_NO_UPDATE_CHECK=1`
  turns the release check off (`wattbit speed on` copies it into the
  login item).
- The **release check** runs on its own short-lived thread, 30 seconds
  after start and then every 12 hours (hourly after a failure), using
  `/usr/bin/curl` against GitHub's "latest release" API. The answer is
  cached in `update.json`; everything that shows the notice reads only
  that file, so no command ever waits on the network.
- **Updates restart the meter.** The pid file records the agent's wattbit
  version. When `wattbit speed on` finds an agent from another version
  (which is what the installer leaves behind right after an update), it
  stops that agent and starts the new one.
- `wattbit speed agent` is the hidden command the login item runs. Its flags
  (`--headless`, `--interval SECONDS` (0.2–60), `--once`) exist for testing
  and are not part of the normal interface. `--once` takes one sample and
  prints the live JSON without staying around.

## Troubleshooting

**The menu bar item doesn't appear**
`wattbit speed status` should show the meter as on. If it's on but there is no
item, check the log:

```sh
tail -5 ~/Library/Application\ Support/wattbit/speed.log
```

Look for a `menu bar item:` line — it reports `visible=true` and the item's
position on screen, which proves macOS accepted it. If the log says
`menu bar unavailable: no GUI session`, you are not in a desktop session:
nothing is broken, usage is still being recorded. If the menu bar is simply
full, macOS hides overflow items: remove or rearrange other menu bar apps
until there is room.

**`wattbit speed on` says the agent did not start**
Read the log (`tail -50 ~/Library/Application\ Support/wattbit/speed.log`); it
explains what the agent did. A leftover data directory from an interrupted
run is the usual cause: `wattbit speed off` and try again. As a last resort,
move `~/Library/Application Support/wattbit` aside to start with a fresh
history.

**`wattbit speed on` must not run as root**
The `speed` commands run inside your login session and refuse `sudo` on
purpose. Re-run them without it: `wattbit speed on`.

**The rates show 0 B/s**
Idle network is idle. Play a video or download something; the numbers move
within a second. If they stay at 0 while you transfer data, check that the
interface you use is counted (see [What is counted](#what-is-counted)) —
traffic inside a VM through a bridged network is the one case that only
crosses excluded interfaces, for example.

**The history lost the start of a download**
The meter only counts from its first sample after `wattbit speed on`; traffic
before that is not in the file. It also flushes to disk every 30 seconds, so
a `kill -9` can lose the last few seconds.

**Where did the older days go?**
That's the 31-day window doing its job. Everything older is deleted from
disk automatically.

## Removing the meter

```sh
wattbit speed off        # stop it and remove the login item
```

That leaves your history in place. To delete it too:

```sh
rm -rf ~/Library/Application\ Support/wattbit
```

Nothing else was ever installed for the meter — no daemon, no system
files, no kernel extensions. To remove all of wattbit (the battery service
too), run the installer with `--uninstall`:

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash -s -- --uninstall
```

## For developers

The speed meter lives in `src/speed/`:

```text
src/speed/
├── mod.rs        module docs, the shared Display snapshot, the stop flag
├── cli glue      (src/cli.rs: SpeedCommand and parse_interval)
├── date.rs       LocalDate (civil-date arithmetic), local-time formatting
├── sampler.rs    NET_RT_IFLIST2 reading, interface filtering, deltas
├── store.rs      usage.json format, atomic saves, retention, quarantine
├── format.rs     decimal-SI units (`1.23 MB/s`, `340 KB/s`)
├── menubar.rs    AppKit/Objective-C FFI: status item, menu, run loop
├── agent.rs      lifecycle: plist, launchd, pid file, sampling loop, socket,
│                 release checks
├── status.rs     assembles the status document (pid + socket + store)
└── render.rs     the text dashboard
```

Design notes:

- **No new crates.** AppKit is reached through `objc_msgSend` with
  hand-written declarations, the same approach `src/ffi.rs` uses for IOKit.
  Every `msg_send!` call states its signature; the wrapper functions are
  `unsafe` and documented.
- **One process per user.** Multiple logged-in users each get their own
  agent, data directory and login item; there is no shared state and no
  privilege boundary to cross, which is why `speed` never needs root.
- **The socket is private and optional.** It carries one line (`status\n`)
  and answers with one JSON object (`LiveState`). If it's missing, `status`
  degrades to the on-disk history. The protocol is not part of any public
  API.
- **Only the pid file decides "on".** A pid is trusted for stopping only if
  its process is alive *and* its executable still matches the running wattbit,
  so a recycled pid can't make `off` kill an unrelated process.
- **Failure is visible, never silent.** A corrupt history is quarantined and
  reported, a missing menu bar is logged, and `wattbit speed status` always
  prints what it knows plus a `warning` when something couldn't be read.

Tests:

```sh
cargo test --lib speed        # unit tests (sampler, store, date, format, agent)
cargo test --test speed       # end-to-end: real binary, private data dir
cargo test --test cli         # command-line parsing
```

The end-to-end tests run the agent with `WATTBIT_SPEED_NO_GUI=1` and a
temporary `WATTBIT_SPEED_DIR`/`HOME`, so they never create a menu bar item and
never touch `~/Library`.
