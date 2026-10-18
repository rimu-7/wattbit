# berry

**Keep your MacBook battery healthy by limiting how far it charges.**

berry stops charging at a limit you choose (for example 80%) and resumes when
the charge drops a little below it. It also shows detailed battery
information, can run your Mac from battery while plugged in, and explains
what your hardware does and doesn't support.

A second, independent tool in the same program — `berry speed` — shows your
live internet speeds in the menu bar and keeps a 31-day usage history. It
needs no `sudo` and works on every Mac.

berry is honest about hardware: if your Mac can't do something, berry says
so and tells you why. It never pretends a setting worked.

- [Requirements](#requirements)
- [Quick start](#quick-start)
- [Will it work on my Mac?](#will-it-work-on-my-mac)
- [Setting a charge limit](#setting-a-charge-limit)
- [Commands](#commands)
- [The internet speed meter](#the-internet-speed-meter)
- [Reading `berry doctor`](#reading-berry-doctor)
- [Reading `berry status`](#reading-berry-status)
- [Troubleshooting](#troubleshooting)
- [Updating and uninstalling](#updating-and-uninstalling)
- [How it works](#how-it-works)
- [For developers](#for-developers)

## Requirements

- A Mac laptop running macOS, with Apple Silicon or an Intel processor
- An administrator account (installing needs `sudo`)
- The Rust toolchain, to build berry: install it from [rustup.rs](https://rustup.rs)

## Quick start

```sh
# 1. Build
cargo build --release

# 2. Install the background service. This also puts `berry` on your PATH.
sudo ./target/release/berry install-service

# 3. Check your Mac for problems and fix anything marked ✘
berry doctor

# 4. Set a limit
sudo berry limit 80

# 5. Check that it's working
berry status
```

If step 4 fails, read [Setting a charge limit](#setting-a-charge-limit). The
most common causes are another battery app still running, or a Mac whose
firmware doesn't allow charge control.

## Will it work on my Mac?

Limiting the charge needs special hardware controls (keys in the System
Management Controller, or SMC). Apple doesn't document them and changes them
between macOS versions. berry looks for every known key at startup and only
uses what it finds and has successfully tested.

| What berry can do | Needs SMC key | When it's missing |
| --- | --- | --- |
| Stop charging at your limit | `CHTE`, or `CH0B` + `CH0C` | Use one of the options below |
| Firmware charge limit (Intel Macs) | `BCLM` | — |
| Run from battery while plugged in | `CHIE` or `CH0I` | `berry adapter` is unavailable |
| Change the MagSafe light colour | `ACLC` | `magsafe-led` is unavailable |

**macOS 27:** the charge-stopping keys have been removed. On macOS 27.0.1
(MacBook Air, Mac16,12) only `CHIE` is left. Other tools see
[the same thing](https://github.com/actuallymentor/battery/pull/478). On
these Macs, berry can't stop charging directly, so `berry status` shows
**Charge control mode: unsupported**. See
[Setting a charge limit](#setting-a-charge-limit) for the alternatives.

To see exactly what your Mac supports, run `berry doctor`.

## Setting a charge limit

### Step 1: Remove other battery apps

Only one battery app can control charging at a time. Run `berry doctor`. If
it lists anything under **Conflict**, remove it with the command doctor
prints. berry recognizes AlDente, batt, Battery Toolkit and the `battery`
command-line tool. While one of them is running, berry pauses and changes
nothing.

### Step 2: Set the limit

```sh
sudo berry limit 80               # stop at 80%, resume below 75%
sudo berry limit 85 --lower 60    # stop at 85%, resume below 60%
sudo berry limit disable          # remove the limit and charge normally
```

The upper limit can be 50–100%. By default, the lower limit is 5% below it.

### If your Mac can't stop charging (macOS 27)

If `berry limit` replies *"this Mac exposes no usable charge-inhibit
control"*, pick **one** of these:

- **Apple's built-in limit (recommended).** macOS 26.4 and later have one in
  **System Settings → Battery → Charging**. It's reliable and needs nothing
  else. Leave berry's limit disabled.
- **berry's adapter fallback.** berry holds the limit by switching the
  charger off and on:

  ```sh
  sudo berry set adapter-fallback on
  sudo berry limit 80
  ```

  At 80% berry switches off charger input, and your Mac runs from its
  battery even though it's plugged in. Below 75% berry switches the charger
  back on. This works, but the battery keeps cycling between 75% and 80%,
  which adds wear. That's why the fallback is off by default.

### Step 3: Confirm it's working

Run `berry status`. You should see:

- no line starting with **Paused**
- **Charge control mode** showing anything other than `unsupported`
- **Upper limit** and **Lower limit** showing your numbers

## Commands

| Command | What it does | Needs `sudo` |
| --- | --- | --- |
| `berry status` | Shows charging, battery, settings and hardware support | no |
| `berry status --json` | The same, as JSON for scripts | no |
| `berry doctor` | Checks your Mac, the service and the settings for problems | no |
| `berry limit [PERCENT] [--lower PERCENT]` | Sets the charge limit (default 80) | yes |
| `berry limit disable` | Removes the limit | yes |
| `berry adapter disable` | Runs the Mac from battery while plugged in | yes |
| `berry adapter enable` | Uses the charger again | yes |
| `berry set OPTION on\|off` | Changes a setting (see below) | yes |
| `berry speed on` | Shows the speed meter in the menu bar and records usage | no |
| `berry speed off` | Hides the meter and stops recording (history is kept) | no |
| `berry speed status` | Live speeds and internet usage for the last 31 days | no |
| `berry install-service` | Installs or updates the background service | yes |
| `berry uninstall-service [--purge]` | Removes it (`--purge` also deletes settings) | yes |

### Settings

| Setting | Default | What it does |
| --- | --- | --- |
| `prevent-idle-sleep` | on | Keeps the Mac awake while it charges toward the limit, so berry can stop it on time |
| `disable-charging-before-sleep` | on | Stops charging before the Mac sleeps, because berry can't watch the battery during sleep |
| `prevent-system-sleep` | off | Stronger version of `prevent-idle-sleep` while charging toward the limit, using the same mechanism as `caffeinate -s`. macOS only applies it on AC power |
| `prevent-sleep-adapter-disabled` | off | Keeps the Mac awake while the charger is switched off |
| `allow-non-root` | on | Lets any user run `berry status` and `berry doctor` |
| `magsafe-led` | off | MagSafe light shows green while the limit holds and orange while charging |
| `adapter-fallback` | off | Holds the limit by switching the charger off (see [above](#if-your-mac-cant-stop-charging-macos-27)) |

Example: `sudo berry set magsafe-led on`

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Error (the message explains it) |
| `2` | Invalid command or value |
| `3` | Your Mac's hardware doesn't support this |
| `4` | Needs `sudo` |
| `5` | Another battery app is in control; remove it first |

## The internet speed meter

`berry speed` shows the current download and upload rates in the menu bar
(`↓ 1.23 MB/s ↑ 340 KB/s`), right next to the system icons, and keeps a
per-day history of your internet usage for the last 31 days. Records older
than that are deleted automatically.

```sh
berry speed on       # show the meter and start recording (no sudo)
berry speed status   # live speeds and the last 31 days of usage
berry speed off      # hide it and stop recording; the history stays
```

It runs as you, in your login session, and starts again by itself at login.
It never needs `sudo`; if you use it anyway, it refuses. The item can be
moved anywhere in the menu bar with ⌘+drag and macOS remembers the spot.

The full guide — units, what is counted (VPNs, VMs, tethering), the data
files, the JSON report, troubleshooting — is in
**[docs/SPEED.md](docs/SPEED.md)**.

## Reading `berry doctor`

Each line starts with a symbol:

| Symbol | Meaning |
| --- | --- |
| ✔ | Working |
| • | Information only, nothing to do |
| ! | Warning: worth knowing, but not broken |
| ✘ | Problem: fix this |

Common lines and what to do about them:

| You see | What it means | What to do |
| --- | --- | --- |
| `✘ Conflict: <app>: running` | Another battery app is controlling the hardware, so berry is paused | Run the removal command printed below it |
| `! Conflict: <app>: installed but not running` | It's installed and could start again | Remove it if you no longer use it |
| `✘ Daemon socket: nothing is listening` | The background service isn't running | `sudo ./target/release/berry install-service` |
| `! Charge inhibit: unsupported` | Your firmware can't stop charging directly | See [If your Mac can't stop charging](#if-your-mac-cant-stop-charging-macos-27) |
| `! Firmware limit (BCLM): unsupported` | Normal on Apple Silicon (it's an Intel feature) | Nothing |
| `! MagSafe LED control: unverified` | The key exists, but berry skipped the write test because something other than macOS was controlling the light. berry never changes the light just to test it | Nothing. It's still usable, and is tested on the next service start if macOS controls the light then |
| `• SMC key …: absent` | That key doesn't exist on your Mac | Nothing |
| `• Config file: … does not exist` | You haven't changed any settings yet | Nothing |

## Reading `berry status`

```text
Charging status:
  Allow charging: ✘ (refreshes can take up to 2 minutes)
    Your Mac will not charge, because your current charge is above the limit.
  Use power adapter: ✔

Battery status:
  Current charge: 80%
  State: not charging
  Full capacity: 8648 mAh / 8694 mAh (99%)
  Charge rate: +0.0 W
  Voltage: 11.89 V

Battery configuration:
  Upper limit: 80%
  Lower limit: 75%
  ...

Hardware compatibility:
  Charge control mode: native
  ...
```

- **Allow charging**: ✔ if the battery may charge now, ✘ if berry is holding
  it at the limit, `unsupported` if berry can't control charging on this Mac.
- **Use power adapter**: ✔ if the Mac uses the charger, ✘ if it runs from
  battery.
- **Paused: …**: another battery app is running; see
  [Step 1](#step-1-remove-other-battery-apps).
- **Full capacity**: how much the battery holds now, compared with when it
  was new. The percentage is the battery's health.
- **Charge rate**: positive while charging, negative while running from
  battery.
- **Charge control mode**: how the limit is enforced. `native` means the
  hardware stops charging; `firmware limit (BCLM)` is the Intel equivalent;
  `adapter fallback` means berry switches the charger off; `unsupported`
  means no limit is possible.
- **✔ (unverified)**: the hardware control exists, but its write test was
  skipped.

## Troubleshooting

**`zsh: command not found: berry`**
Run `sudo ./target/release/berry install-service` again from the project
folder. It creates `/usr/local/bin/berry`. If you see that path in a warning
instead, another file is already using the name, and you can run berry as
`/Library/PrivilegedHelperTools/berryd`.

**`the berry daemon is not running`**
Install the service: `sudo ./target/release/berry install-service`. If it is
installed, check the log: `tail -50 /var/log/berry.log`.

**`requires root; re-run with sudo`** (exit code 4)
Commands that change anything need `sudo`, for example `sudo berry limit 80`.

**`other charge limiters are controlling the same hardware`** (exit code 5)
Remove the apps it lists, using the commands it prints. berry resumes on its
own once they're gone.

**`this Mac exposes no usable charge-inhibit control`** (exit code 3)
Your firmware can't stop charging directly. See
[If your Mac can't stop charging](#if-your-mac-cant-stop-charging-macos-27).

**The battery went past my limit**
Check `berry status` for a **Paused** line or a **Last error** line. Also
check whether the Mac was asleep: berry can't act during sleep, which is why
`disable-charging-before-sleep` is on by default.

**Watching what berry does**
`tail -f /var/log/berry.log` shows every decision as it happens, for
example: limit reached, charging stopped, charger switched back on.

**The speed meter misbehaves**
The menu bar item, the history file and the log have their own guide:
[docs/SPEED.md](docs/SPEED.md#troubleshooting). Start with
`berry speed status`.

## Updating and uninstalling

**Update:** pull the new code, then run:

```sh
cargo build --release
sudo ./target/release/berry install-service
```

This stops the old service, replaces it and starts the new one. Your
settings are kept.

**Uninstall:**

```sh
sudo berry uninstall-service           # keeps your settings in /etc/berry.json
sudo berry uninstall-service --purge   # also deletes them
```

Normal charging is restored immediately.

### What gets installed

| Path | What it is |
| --- | --- |
| `/Library/PrivilegedHelperTools/berryd` | The berry program, owned by root |
| `/usr/local/bin/berry` | Link to it, so you can type `berry` |
| `/Library/LaunchDaemons/com.berry.daemon.plist` | Starts berry's background service at boot |
| `/etc/berry.json` | Your settings (created when you first change one) |
| `/var/log/berry.log` | Log |
| `/var/run/berry.sock` | How the `berry` command talks to the service |
| `~/Library/LaunchAgents/com.berry.speed.plist` | Login item for the speed meter, only while `berry speed on` is active (per user) |
| `~/Library/Application Support/berry/` | Speed-meter usage history and log (per user, mode 0700) |

## How it works

berry has two parts. The **service** (`berryd`) runs in the background as
root and is the only part that touches the hardware. The **`berry`
command** talks to the service and doesn't need special rights to read
status.

### The limit

With an upper limit *U* and a lower limit *L*:

| State | When | Charging |
| --- | --- | --- |
| `ChargingPermitted` | charge is below *L*, or between the limits while charging up | allowed |
| `ChargingSuppressed` | charge has reached *U* | stopped |
| `WaitingToResumeCharging` | charge is between the limits after reaching *U* | stopped |

The gap between the limits stops charging from switching on and off at every
1% change. macOS notifies berry whenever the charge level changes or the
charger is plugged in, and berry reacts immediately. It also re-checks every
30 seconds as a backup. Every hardware change is read back to confirm it
took effect.

### Safety

- **Normal charging returns when berry stops.** Stopping or uninstalling the
  service switches charging and the charger back on. If another battery app
  is present, berry undoes only its own changes.
- **The charger always comes back at 10%.** If the charger is switched off
  and the battery drops to 10%, berry switches it back on, and it refuses to
  switch it off at or below 10%. The adapter fallback never holds the charger
  off during sleep.
- **No guessing.** berry only writes to a hardware key while it holds one of
  two known values, and it tests write access by writing back the value it
  just read, which changes nothing.
- **No hanging.** Every hardware call and connection has a time limit.
- **Settings can't be corrupted.** `/etc/berry.json` is written to a
  temporary file and swapped in at once. Invalid values are rejected when
  it's loaded.
- **No fighting.** While another battery app runs, berry pauses (see
  [Step 1](#step-1-remove-other-battery-apps)).

### Security

Every request to the service is checked with the caller's user ID. Only root
(`sudo`) can change anything. Other users can read status and run doctor,
and only while `allow-non-root` is on; turning it off also restricts the
socket to root. The service only runs a root-owned copy of the program, never
one in a user-writable folder.

## For developers

### Layout

```text
src/
├── main.rs            entry point and command dispatch
├── cli.rs             command-line definitions and validation
├── client.rs          socket client and the status display
├── protocol.rs        JSON-RPC 2.0 messages and error codes
├── daemon/
│   ├── mod.rs         socket server, caller authorization, event loop
│   └── controller.rs  policy evaluation and hardware reconciliation
├── policy.rs          limits and the charging state machine
├── config.rs          atomic settings file
├── capabilities.rs    SMC key probing and the feature table
├── conflicts.rs       detection of other battery apps
├── hardware.rs        verified charge, adapter, LED and BCLM control
├── smc/
│   ├── mod.rs         AppleSMC driver (IOConnectCallStructMethod)
│   └── keys.rs        key names and the exact bytes written to each
├── battery.rs         battery data from IOPowerSources and AppleSmartBattery
├── power.rs           sleep/wake and power-source notifications, sleep prevention
├── doctor.rs          diagnostic checks
├── service.rs         LaunchDaemon install and uninstall
├── speed/             internet speed meter (see docs/SPEED.md)
└── ffi.rs             IOKit declarations taken from the macOS SDK headers
```

berry uses only native frameworks (IOKit and CoreFoundation). The one
external program it runs is `/bin/launchctl`, during install and uninstall:
macOS has no supported C API that lets a command-line tool load a system
service.

Control values are stored as exact bytes rather than numbers, because SMC
integers are little-endian on Apple Silicon but big-endian on Intel.

### Protocol

The `berry` command sends one JSON-RPC 2.0 request per connection to
`/var/run/berry.sock`, as a single line:

```json
{"jsonrpc":"2.0","id":1,"method":"set_limit","params":{"upper":80,"lower":75}}
{"jsonrpc":"2.0","id":1,"result":{"kind":"ack","message":"Charge limit set: ..."}}
```

Methods: `status`, `doctor`, `set_limit`, `disable_limit`, `set_adapter`,
`set_option`. Besides the standard JSON-RPC errors, berry uses `-32001`
(unauthorized), `-32002` (unsupported), `-32003` (hardware failure), `-32004`
(timeout) and `-32005` (another battery app running). `error.data.kind` names
the category.

### Testing

```sh
cargo test                                            # all automated tests
cargo test --test speed                               # speed meter end to end
cargo test --test hardware -- --ignored --nocapture   # read-only checks of this Mac
```

To try changes without installing, run the service in read-only mode. It
doesn't need root, reads the hardware but never changes it, and is what the
end-to-end tests use:

```sh
cargo run -- daemon run --read-only --socket /tmp/berry.sock --config /tmp/berry.json
cargo run -- status --socket /tmp/berry.sock
```

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, the tests and a
release build on macOS.
