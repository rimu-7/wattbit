# wattbit

**Keep your MacBook battery healthy, and see your internet speed in the menu bar.**

wattbit is one small program with two tools:

- **Charge limiting.** wattbit stops charging at a limit you choose (for
  example 80%) and resumes when the charge drops a little below it. It also
  shows detailed battery information, can run your Mac from battery while
  plugged in, and explains what your hardware does and doesn't support.
- **Speed meter.** `wattbit speed` shows your live download and upload
  speeds in the menu bar and keeps a 31-day usage history. It needs no
  `sudo` and works on every Mac.

One command installs both, and the same command updates them later.

wattbit is honest about hardware: if your Mac can't do something, wattbit
says so and tells you why. It never pretends a setting worked.

- [Install](#install)
- [Requirements](#requirements)
- [Quick start](#quick-start)
- [Will it work on my Mac?](#will-it-work-on-my-mac)
- [Setting a charge limit](#setting-a-charge-limit)
- [Commands](#commands)
- [The internet speed meter](#the-internet-speed-meter)
- [Reading `wattbit doctor`](#reading-wattbit-doctor)
- [Reading `wattbit status`](#reading-wattbit-status)
- [Troubleshooting](#troubleshooting)
- [Updating and uninstalling](#updating-and-uninstalling)
- [Coming from berry](#coming-from-berry)
- [How it works](#how-it-works)
- [For developers](#for-developers)

## Install

Open Terminal and run:

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
```

The installer:

1. downloads the latest release for your Mac (one universal binary for
   Apple Silicon and Intel) and checks its SHA-256 checksum,
2. asks for your password once and installs the battery service,
3. puts the `wattbit` command on your PATH,
4. turns on the speed meter in the menu bar,
5. moves an older install over, if you had one (see
   [Coming from berry](#coming-from-berry)).

Run it as yourself, not with `sudo`; it asks for your password when it needs
it. Running it again is always safe: it updates or repairs the installation
and keeps your settings and usage history.

Options go after `bash -s --`:

| Option | What it does |
| --- | --- |
| `--no-speed` | Install without turning on the speed meter |
| `--version 0.2.0` | Install a specific release instead of the latest |
| `--from-source` | Build the latest code with Rust instead of downloading a release |
| `--uninstall` | Remove wattbit (keeps settings and usage history) |
| `--uninstall --purge` | Remove wattbit, its settings and its usage history |

For example: `curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash -s -- --no-speed`.

## Requirements

- A Mac laptop running macOS, with Apple Silicon or an Intel processor
- An administrator account (the battery service needs your password once)
- Nothing else. Rust is only needed for `--from-source` or for working on
  wattbit itself.

## Quick start

```sh
# 1. Install (or update) both tools
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash

# 2. Check your Mac for problems and fix anything marked ✘
wattbit doctor

# 3. Set a charge limit
sudo wattbit limit 80

# 4. Check that it's working
wattbit status
```

The speed meter is already in your menu bar after step 1.

If step 3 fails, read [Setting a charge limit](#setting-a-charge-limit). The
most common causes are another battery app still running, or a Mac whose
firmware doesn't allow charge control.

## Will it work on my Mac?

Limiting the charge needs special hardware controls (keys in the System
Management Controller, or SMC). Apple doesn't document them and changes them
between macOS versions. wattbit looks for every known key at startup and only
uses what it finds and has successfully tested.

| What wattbit can do | Needs SMC key | When it's missing |
| --- | --- | --- |
| Stop charging at your limit | `CHTE`, or `CH0B` + `CH0C` | Use one of the options below |
| Firmware charge limit (Intel Macs) | `BCLM` | — |
| Run from battery while plugged in | `CHIE` or `CH0I` | `wattbit adapter` is unavailable |
| Change the MagSafe light colour | `ACLC` | `magsafe-led` is unavailable |

**macOS 27:** the charge-stopping keys have been removed. On macOS 27.0.1
(MacBook Air, Mac16,12) only `CHIE` is left. Other tools see
[the same thing](https://github.com/actuallymentor/battery/pull/478). On
these Macs, wattbit can't stop charging directly, so `wattbit status` shows
**Charge control mode: unsupported**. See
[Setting a charge limit](#setting-a-charge-limit) for the alternatives.

To see exactly what your Mac supports, run `wattbit doctor`.

## Setting a charge limit

### Step 1: Remove other battery apps

Only one battery app can control charging at a time. Run `wattbit doctor`. If
it lists anything under **Conflict**, remove it with the command doctor
prints. wattbit recognizes AlDente, batt, Battery Toolkit, the `battery`
command-line tool and berry (wattbit's old name). While one of them is
running, wattbit pauses and changes nothing.

### Step 2: Set the limit

```sh
sudo wattbit limit 80               # stop at 80%, resume below 75%
sudo wattbit limit 85 --lower 60    # stop at 85%, resume below 60%
sudo wattbit limit disable          # remove the limit and charge normally
```

The upper limit can be 50–100%. By default, the lower limit is 5% below it.

### If your Mac can't stop charging (macOS 27)

If `wattbit limit` replies *"this Mac exposes no usable charge-inhibit
control"*, pick **one** of these:

- **Apple's built-in limit (recommended).** macOS 26.4 and later have one in
  **System Settings → Battery → Charging**. It's reliable and needs nothing
  else. Leave wattbit's limit disabled.
- **wattbit's adapter fallback.** wattbit holds the limit by switching the
  charger off and on:

  ```sh
  sudo wattbit set adapter-fallback on
  sudo wattbit limit 80
  ```

  At 80% wattbit switches off charger input, and your Mac runs from its
  battery even though it's plugged in. Below 75% wattbit switches the charger
  back on. This works, but the battery keeps cycling between 75% and 80%,
  which adds wear. That's why the fallback is off by default.

### Step 3: Confirm it's working

Run `wattbit status`. You should see:

- no line starting with **Paused**
- **Charge control mode** showing anything other than `unsupported`
- **Upper limit** and **Lower limit** showing your numbers

## Commands

| Command | What it does | Needs `sudo` |
| --- | --- | --- |
| `wattbit status` | Shows charging, battery, settings and hardware support | no |
| `wattbit status --json` | The same, as JSON for scripts | no |
| `wattbit doctor` | Checks your Mac, the service and the settings for problems | no |
| `wattbit limit [PERCENT] [--lower PERCENT]` | Sets the charge limit (default 80) | yes |
| `wattbit limit disable` | Removes the limit | yes |
| `wattbit adapter disable` | Runs the Mac from battery while plugged in | yes |
| `wattbit adapter enable` | Uses the charger again | yes |
| `wattbit set OPTION on\|off` | Changes a setting (see below) | yes |
| `wattbit speed on` | Shows the speed meter in the menu bar and records usage | no |
| `wattbit speed off` | Hides the meter and stops recording (history is kept) | no |
| `wattbit speed status` | Live speeds and internet usage for the last 31 days | no |
| `wattbit install-service` | Installs or repairs the background service (the installer runs this) | yes |
| `wattbit uninstall-service [--purge]` | Removes it (`--purge` also deletes settings) | yes |

### Settings

| Setting | Default | What it does |
| --- | --- | --- |
| `prevent-idle-sleep` | on | Keeps the Mac awake while it charges toward the limit, so wattbit can stop it on time |
| `disable-charging-before-sleep` | on | Stops charging before the Mac sleeps, because wattbit can't watch the battery during sleep |
| `prevent-system-sleep` | off | Stronger version of `prevent-idle-sleep` while charging toward the limit, using the same mechanism as `caffeinate -s`. macOS only applies it on AC power |
| `prevent-sleep-adapter-disabled` | off | Keeps the Mac awake while the charger is switched off |
| `allow-non-root` | on | Lets any user run `wattbit status` and `wattbit doctor` |
| `magsafe-led` | off | MagSafe light shows green while the limit holds and orange while charging |
| `adapter-fallback` | off | Holds the limit by switching the charger off (see [above](#if-your-mac-cant-stop-charging-macos-27)) |

Example: `sudo wattbit set magsafe-led on`

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

`wattbit speed` shows the current download and upload rates in the menu bar
(`↓ 1.23 MB/s ↑ 340 KB/s`), right next to the system icons, and keeps a
per-day history of your internet usage for the last 31 days. Records older
than that are deleted automatically.

```sh
wattbit speed on       # show the meter and start recording (no sudo)
wattbit speed status   # live speeds and the last 31 days of usage
wattbit speed off      # hide it and stop recording; the history stays
```

The installer turns it on for you. It runs as you, in your login session,
and starts again by itself at login. It never needs `sudo`; if you use it
anyway, it refuses. The item can be moved anywhere in the menu bar with
⌘+drag and macOS remembers the spot.

Click the item to see today's and the month's totals. When a new wattbit
release is out, the menu opens with an **Update available** warning, the
exact command to run in Terminal, and **Copy Update Command**, which puts
that command on your clipboard.

The full guide — units, what is counted (VPNs, VMs, tethering), the data
files, the JSON report, troubleshooting — is in
**[docs/SPEED.md](docs/SPEED.md)**.

## Reading `wattbit doctor`

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
| `✘ Conflict: <app>: running` | Another battery app is controlling the hardware, so wattbit is paused | Run the removal command printed below it |
| `! Conflict: <app>: installed but not running` | It's installed and could start again | Remove it if you no longer use it |
| `✘ Daemon socket: nothing is listening` | The background service isn't running | Run the [install command](#install) again |
| `! Version: … is available` | A newer wattbit is out | Run the command it prints (the install command) |
| `! Daemon version: …` | An update didn't finish: the service runs a different version than the `wattbit` command | Run the [install command](#install) again |
| `! Old berry files: …` | Files from berry, wattbit's old name, are still there | Run the [install command](#install) again; it moves your settings over and removes them |
| `• Speed meter: off` | The menu bar speed meter isn't running | `wattbit speed on` if you want it |
| `! Charge inhibit: unsupported` | Your firmware can't stop charging directly | See [If your Mac can't stop charging](#if-your-mac-cant-stop-charging-macos-27) |
| `! Firmware limit (BCLM): unsupported` | Normal on Apple Silicon (it's an Intel feature) | Nothing |
| `! MagSafe LED control: unverified` | The key exists, but wattbit skipped the write test because something other than macOS was controlling the light. wattbit never changes the light just to test it | Nothing. It's still usable, and is tested on the next service start if macOS controls the light then |
| `• SMC key …: absent` | That key doesn't exist on your Mac | Nothing |
| `• Config file: … does not exist` | You haven't changed any settings yet | Nothing |

## Reading `wattbit status`

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

- **Allow charging**: ✔ if the battery may charge now, ✘ if wattbit is holding
  it at the limit, `unsupported` if wattbit can't control charging on this Mac.
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
  `adapter fallback` means wattbit switches the charger off; `unsupported`
  means no limit is possible.
- **✔ (unverified)**: the hardware control exists, but its write test was
  skipped.

## Troubleshooting

**`zsh: command not found: wattbit`**
Run the [install command](#install) again. It creates
`/usr/local/bin/wattbit`. If the installer warns that this path is taken,
another file is already using the name, and you can run wattbit as
`/Library/PrivilegedHelperTools/wattbitd`.

**`the wattbit daemon is not running`**
Run the [install command](#install) again; it reinstalls the service and
waits until the daemon answers. If it still fails, check the log:
`tail -50 /var/log/wattbit.log`.

**The installer says "no wattbit release is published yet"**
Either no release has been tagged yet, or GitHub can't be reached. Install
Rust from [rustup.rs](https://rustup.rs) and run the installer again; it
then builds wattbit from source automatically.

**`requires root; re-run with sudo`** (exit code 4)
Commands that change anything need `sudo`, for example `sudo wattbit limit 80`.

**`other charge limiters are controlling the same hardware`** (exit code 5)
Remove the apps it lists, using the commands it prints. wattbit resumes on its
own once they're gone.

**`this Mac exposes no usable charge-inhibit control`** (exit code 3)
Your firmware can't stop charging directly. See
[If your Mac can't stop charging](#if-your-mac-cant-stop-charging-macos-27).

**The battery went past my limit**
Check `wattbit status` for a **Paused** line or a **Last error** line. Also
check whether the Mac was asleep: wattbit can't act during sleep, which is why
`disable-charging-before-sleep` is on by default.

**Watching what wattbit does**
`tail -f /var/log/wattbit.log` shows every decision as it happens, for
example: limit reached, charging stopped, charger switched back on.

**The speed meter misbehaves**
The menu bar item, the history file and the log have their own guide:
[docs/SPEED.md](docs/SPEED.md#troubleshooting). Start with
`wattbit speed status`.

## Updating and uninstalling

### Knowing when there's an update

While the speed meter runs, it asks GitHub for the latest wattbit release
every 12 hours (one anonymous request to `api.github.com`; nothing about
your Mac is sent). When a newer release exists:

- the speed meter's menu opens with **⚠️ Update available**, the command to
  run, and **Copy Update Command**;
- `wattbit status`, `wattbit speed status` and `wattbit doctor` print the
  same notice and command.

To turn the check off, start the meter with
`WATTBIT_NO_UPDATE_CHECK=1 wattbit speed on`. The setting is stored in its
login item.

### Updating

Run the install command again:

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
```

It downloads and verifies the new release. Then it stops the battery
service, which restores normal charging for a moment, starts the new
service, and waits until it answers. Last, it restarts the speed meter on
the new version. Your charge limit, settings and usage history are kept.
If the download or the checksum fails, the installer stops before changing
anything, and the installed version keeps running.

### Uninstalling

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash -s -- --uninstall           # keeps /etc/wattbit.json and your usage history
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash -s -- --uninstall --purge   # deletes them too
```

Normal charging is restored immediately. Without the installer, the same
is `wattbit speed off` followed by `sudo wattbit uninstall-service [--purge]`.

### What gets installed

| Path | What it is |
| --- | --- |
| `/Library/PrivilegedHelperTools/wattbitd` | The wattbit program (a universal binary), owned by root |
| `/usr/local/bin/wattbit` | Link to it, so you can type `wattbit` |
| `/Library/LaunchDaemons/com.wattbit.daemon.plist` | Starts wattbit's background service at boot |
| `/etc/wattbit.json` | Your settings (created when you first change one) |
| `/var/log/wattbit.log` | Log |
| `/var/run/wattbit.sock` | How the `wattbit` command talks to the service |
| `~/Library/LaunchAgents/com.wattbit.speed.plist` | Login item for the speed meter, only while `wattbit speed on` is active (per user) |
| `~/Library/Application Support/wattbit/` | Speed-meter usage history, log and the last update check (per user, mode 0700) |

## Coming from berry

wattbit used to be called **berry**. The installer moves a berry
installation over automatically, so there is nothing to do by hand:

| berry | becomes |
| --- | --- |
| `/etc/berry.json` (your settings and charge limit) | moved to `/etc/wattbit.json` |
| `~/Library/Application Support/berry/speed.json` (usage history) | merged into `~/Library/Application Support/wattbit/speed.json` |
| `com.berry.daemon`, `berryd`, `/usr/local/bin/berry`, `/var/log/berry.log`, `/var/run/berry.sock` | stopped and removed |
| `com.berry.speed` login item and the rest of berry's speed folder | stopped and removed |

The old daemon is stopped *before* the new one starts, so the two never
control charging at the same time. If a berry daemon is still running for
any reason, wattbit treats it like any other battery app and pauses; `wattbit
doctor` shows it and any leftover files. Files berry didn't create are
never deleted.

## How it works

wattbit has two parts. The **service** (`wattbitd`) runs in the background as
root and is the only part that touches the hardware. The **`wattbit`
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
1% change. macOS notifies wattbit whenever the charge level changes or the
charger is plugged in, and wattbit reacts immediately. It also re-checks every
30 seconds as a backup. Every hardware change is read back to confirm it
took effect.

### Safety

- **Normal charging returns when wattbit stops.** Stopping or uninstalling the
  service switches charging and the charger back on. If another battery app
  is present, wattbit undoes only its own changes.
- **The charger always comes back at 10%.** If the charger is switched off
  and the battery drops to 10%, wattbit switches it back on, and it refuses to
  switch it off at or below 10%. The adapter fallback never holds the charger
  off during sleep.
- **No guessing.** wattbit only writes to a hardware key while it holds one of
  two known values, and it tests write access by writing back the value it
  just read, which changes nothing.
- **No hanging.** Every hardware call and connection has a time limit.
- **Settings can't be corrupted.** `/etc/wattbit.json` is written to a
  temporary file and swapped in at once. Invalid values are rejected when
  it's loaded.
- **No fighting.** While another battery app runs, wattbit pauses (see
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
├── service.rs         LaunchDaemon install, update and uninstall
├── legacy.rs          moving a berry installation over
├── update.rs          release checks and the update notice
├── speed/             internet speed meter (see docs/SPEED.md)
└── ffi.rs             IOKit declarations taken from the macOS SDK headers
install.sh             the one-line installer and updater
```

wattbit uses only native frameworks (IOKit, CoreFoundation and AppKit). It
runs two external programs: `/bin/launchctl` to load and unload its
services (macOS has no supported C API that lets a command-line tool load
a system service), and `/usr/bin/curl` for the speed meter's release
check, so it needs no TLS library of its own. The root daemon never
touches the network.

Control values are stored as exact bytes rather than numbers, because SMC
integers are little-endian on Apple Silicon but big-endian on Intel.

### Protocol

The `wattbit` command sends one JSON-RPC 2.0 request per connection to
`/var/run/wattbit.sock`, as a single line:

```json
{"jsonrpc":"2.0","id":1,"method":"set_limit","params":{"upper":80,"lower":75}}
{"jsonrpc":"2.0","id":1,"result":{"kind":"ack","message":"Charge limit set: ..."}}
```

Methods: `status`, `doctor`, `set_limit`, `disable_limit`, `set_adapter`,
`set_option`. Besides the standard JSON-RPC errors, wattbit uses `-32001`
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
cargo run -- daemon run --read-only --socket /tmp/wattbit.sock --config /tmp/wattbit.json
cargo run -- status --socket /tmp/wattbit.sock
```

CI runs a syntax check of `install.sh`, `cargo fmt --check`,
`cargo clippy -- -D warnings`, the tests and a release build on macOS.

### Releasing

1. Bump `version` in `Cargo.toml` and run `cargo build` so `Cargo.lock`
   follows.
2. Commit, then tag and push: `git tag v0.2.0 && git push origin main v0.2.0`.

The [release workflow](.github/workflows/release.yml) checks that the tag
matches `Cargo.toml` and runs the tests. It then builds a universal
(arm64 + x86_64), ad-hoc-signed binary and publishes
`wattbit-macos-universal.tar.gz` with its `.sha256` file. From then on,
the installer installs that release, and every running speed meter offers
the update within 12 hours.
