# berry

Battery charge limiting, power adapter control and battery telemetry for macOS,
written in Rust against IOKit and the System Management Controller (SMC).

berry has two parts:

- **`berryd`**: a root daemon (`berry daemon run`) that owns all SMC and IOKit
  access, runs the charge-limit state machine and persists configuration.
- **`berry`**: an unprivileged CLI that talks to the daemon over a Unix domain
  socket at `/var/run/berry.sock`.

berry never pretends. Each control is enabled only after the SMC keys it needs
have been found and, when running as root, write-tested. Anything the
hardware cannot do is reported as `unsupported`, along with the reason.

## Hardware support

Charge limiting depends on undocumented SMC keys whose names change between
firmware generations. berry probes for each of them at startup:

| Feature | SMC keys (in order of preference) | Notes |
| --- | --- | --- |
| Charge inhibit (`native` mode) | `CHTE`, then `CH0B` + `CH0C` | berry runs the hysteresis loop |
| Firmware limit | `BCLM` | Intel firmware enforces the upper limit itself |
| Power adapter control | `CHIE`, then `CH0I` | Mac runs from battery while plugged in |
| MagSafe LED | `ACLC` | Optional status colour while a limit is set |

**macOS 27:** on the firmware tested so far (macOS 27.0.1, Mac16,12), `CHTE`,
`CH0B`, `CH0C` and `BCLM` are all absent and only `CHIE` responds. This
matches [community reports](https://github.com/actuallymentor/battery/pull/478).
There, berry reports charge control as `unsupported` and does not fake it. Two
options remain:

- **macOS 26.4 and later include a built-in charge limit** in System Settings →
  Battery, which is the most robust choice where it is available.
- **Adapter fallback** (opt-in): `sudo berry set adapter-fallback on` lets
  berry hold the limit by bypassing the adapter, so the Mac runs from its
  battery between the upper and lower limits. That costs battery cycles, so
  it is off by default and reported as its own mode (`adapter fallback`).

Run `berry doctor` to see exactly what your Mac supports.

## Installation

Requires macOS and a Rust toolchain (edition 2021).

```sh
cargo build --release
sudo ./target/release/berry install-service
berry doctor     # check for problems first
berry status
```

`install-service`:

- copies the binary to `/Library/PrivilegedHelperTools/berryd`, which is owned
  by root, so the daemon never runs a user-writable file;
- links `/usr/local/bin/berry` to it, so `berry` is on your `PATH`;
- writes `/Library/LaunchDaemons/com.berry.daemon.plist` and loads it.

Logs go to `/var/log/berry.log`.

To update after pulling new code, rebuild and run
`sudo ./target/release/berry install-service` again. It stops the old daemon,
replaces the binary and starts the new one.

To remove berry, run `sudo berry uninstall-service`. That restores factory
charging and removes the daemon, plist and `PATH` link. `/etc/berry.json` is
kept; add `--purge` to delete it too.

## Other charge limiters

batt, AlDente, actuallymentor's `battery` and Battery Toolkit write the same
SMC keys as berry. If two of these tools run at once, they override each
other and the charging state oscillates.

berry checks for them every time it evaluates the policy, including from the
root daemon, which can see every process. While one is running:

- berry **pauses**: it reads hardware state but writes nothing;
- `berry status` names the tools and their process IDs;
- `berry limit` and `berry adapter` are refused with exit status `5`, along
  with the exact commands to stop and remove each tool;
- `berry doctor` also lists tools that are installed but not running.

Remove the other tools, and berry resumes control automatically on its next
evaluation. When it exits while another tool is present, berry undoes only
the changes it made itself.

## Getting a working limit on macOS 27

1. Run `berry doctor` and remove every tool listed under **Conflict**.
2. If doctor reports **Charge inhibit: unsupported** (as on macOS 27
   firmware), choose one of:
   - Apple's built-in limit: System Settings → Battery → Charging. Leave
     berry's limit disabled.
   - berry's adapter fallback:

     ```sh
     sudo berry set adapter-fallback on
     sudo berry limit 80
     ```

     The Mac runs from its battery between 75% and 80% while plugged in.
3. Run `berry status` and confirm that **Charge control mode** is not
   `unsupported` and that no **Paused** line appears.

## Usage

| Command | Description | Privileges |
| --- | --- | --- |
| `berry status [--json]` | Charging, battery, configuration and hardware dashboard | user* |
| `berry limit [PERCENT] [--lower PERCENT]` | Stop charging at `PERCENT` (50–100, default 80); resume below `--lower` (default `PERCENT − 5`) | root |
| `berry limit disable` | Remove the limit and restore factory charging | root |
| `berry adapter enable\|disable` | Use or bypass the power adapter | root |
| `berry set OPTION on\|off` | Change a setting (see below) | root |
| `berry doctor` | Diagnose architecture, SMC keys, daemon, config and service | user |
| `berry daemon run [--config PATH] [--read-only]` | Run the daemon in the foreground (launchd uses this) | root |
| `berry install-service` / `uninstall-service [--purge]` | Manage the LaunchDaemon | root |

\* Unless `allow-non-root` is off. Every command accepts `--socket PATH`.

Settings for `berry set`:

| Option | Default | Effect |
| --- | --- | --- |
| `prevent-idle-sleep` | on | Hold an idle-sleep assertion while charging toward the limit, so the daemon can stop at the limit |
| `disable-charging-before-sleep` | on | Inhibit charging before system sleep while a limit is set, because the daemon cannot run during sleep |
| `prevent-system-sleep` | off | Hold a system-sleep assertion while charging toward the limit |
| `prevent-sleep-adapter-disabled` | off | Hold an idle-sleep assertion while the adapter is bypassed |
| `allow-non-root` | on | Let non-root users run `status` and `doctor` |
| `magsafe-led` | off | Show green while the limit holds and orange while charging |
| `adapter-fallback` | off | Hold the limit by bypassing the adapter when no charge-inhibit key exists |

Exit status: `0` success, `1` error, `2` invalid usage, `3` not supported by
this Mac's hardware, `4` insufficient privileges, `5` another charge limiter
is running.

## How the limit works

With an upper limit *U* and a lower limit *L* (default *L* = max(0, *U* − 5)):

| State | Condition | Charging |
| --- | --- | --- |
| `ChargingSuppressed` | charge ≥ *U* | held off |
| `WaitingToResumeCharging` | *L* ≤ charge < *U*, after being suppressed | held off |
| `ChargingPermitted` | charge < *L*, or below *U* while already charging | allowed |

The daemon subscribes to IOPowerSources change notifications. It re-evaluates
as the charge level changes and when the adapter is plugged in or removed,
so charging stops at the limit rather than overshooting it. It also
re-evaluates every 30 seconds as a fallback, on wake, and after every change
to its settings. Each SMC write is read back to confirm it took effect.

## Safety

- **Fail-safe exit.** On `SIGTERM`/`SIGINT` (including `uninstall-service`
  and system shutdown) the daemon re-enables charging and the adapter, returns
  the LED to the system and releases its power assertions. On start it
  reconciles the hardware with the saved configuration.
- **Adapter safety floor.** A bypassed adapter is re-enabled automatically at
  10% charge, and `berry adapter disable` is refused at or below it. The
  adapter-fallback bypass is never held across sleep, because the daemon
  cannot watch the battery while the Mac sleeps.
- **Only known states are written.** A control is used only while its keys
  hold one of its two known byte patterns. Write access is tested by writing
  back the value just read, which doesn't change hardware state.
- **Bounded I/O.** Every hardware call has a 5 s timeout and every socket read
  and write has a timeout. Messages are capped at 256 KiB, and at most 16
  clients are served at once.
- **Atomic configuration.** `/etc/berry.json` is written to a temporary file,
  fsynced and renamed into place. Out-of-range thresholds are rejected when
  the file is loaded.

## Security model

The socket is world-connectable (`0666`) while `allow-non-root` is on and
owner-only (`0600`) otherwise. Access is enforced per request with
`getpeereid(2)`: root may do anything; other users may only make read calls
(`status`, `doctor`), and only while `allow-non-root` is on. To change
anything, use `sudo`.

## Architecture

```text
src/
├── main.rs            entry point and command dispatch
├── cli.rs             clap definitions and argument validation
├── client.rs          socket client and status dashboard renderer
├── protocol.rs        JSON-RPC 2.0 messages, error codes, framing
├── daemon/
│   ├── mod.rs         socket server, getpeereid authorization, event loop
│   └── controller.rs  policy evaluation and hardware reconciliation
├── policy.rs          thresholds and the hysteresis state machine
├── config.rs          atomic JSON configuration store
├── capabilities.rs    SMC key probing and the feature matrix
├── conflicts.rs       detection of other charge-limit tools
├── hardware.rs        verified charge/adapter/LED/BCLM control
├── smc/
│   ├── mod.rs         AppleSMC user client (IOConnectCallStructMethod)
│   └── keys.rs        key names and exact byte patterns per control
├── battery.rs         IOPowerSources + AppleSmartBattery telemetry
├── power.rs           IOPM assertions, sleep/wake and power-source notifications
├── doctor.rs          diagnostic checks
├── service.rs         LaunchDaemon plist, install and uninstall
└── ffi.rs             IOKit declarations transcribed from the SDK headers
```

berry uses native frameworks only: IOKit and CoreFoundation through
`core-foundation` and `mach2`. The one external process is `/bin/launchctl`
during `install-service`/`uninstall-service`, because macOS has no supported
C API that lets a command-line tool load a system LaunchDaemon.

Control values are stored as exact byte patterns rather than numbers. SMC
integers are little-endian on Apple Silicon (for example `B0AV`) but
big-endian on Intel, so berry never re-encodes a value it writes.

### IPC protocol

One newline-terminated JSON-RPC 2.0 request and one response per connection:

```json
{"jsonrpc":"2.0","id":1,"method":"set_limit","params":{"upper":80,"lower":75}}
{"jsonrpc":"2.0","id":1,"result":{"kind":"ack","message":"Charge limit set: ..."}}
```

Methods: `status`, `doctor`, `set_limit`, `disable_limit`, `set_adapter`,
`set_option`. Errors use the standard JSON-RPC codes, plus `-32001`
(unauthorized), `-32002` (unsupported), `-32003` (hardware), `-32004`
(timeout) and `-32005` (conflict with another charge limiter), with
`error.data.kind` naming the category.

## Development

```sh
cargo test                                          # unit, protocol, CLI and end-to-end tests
cargo test --test hardware -- --ignored --nocapture # read-only probes of this Mac's SMC and battery
cargo run -- daemon run --read-only --socket /tmp/berry.sock --config /tmp/berry.json
cargo run -- status --socket /tmp/berry.sock
```

`--read-only` runs the daemon without root. It reads hardware but never writes
to it and holds no power assertions, which keeps development and the
end-to-end tests from changing the machine's charging state.
