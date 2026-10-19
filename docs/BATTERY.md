# Charge limiting guide

Everything about wattbit's battery side: setting a limit, reading
`wattbit status`, what to do when a Mac can't stop charging, every setting,
and the safeguards behind it. For installing wattbit, see the
[README](../README.md).

- [Before you start](#before-you-start)
- [Setting a limit](#setting-a-limit)
- [Reading `wattbit status`](#reading-wattbit-status)
- [If your Mac can't stop charging](#if-your-mac-cant-stop-charging)
- [Settings](#settings)
- [Running on battery while plugged in](#running-on-battery-while-plugged-in)
- [Reading `wattbit doctor`](#reading-wattbit-doctor)
- [How it works](#how-it-works)
- [Files](#files)
- [Troubleshooting](#troubleshooting)

## Before you start

Only one app can control charging at a time. wattbit recognizes **AlDente,
batt, Battery Toolkit** and the **`battery`** command-line tool. While one of
them runs, wattbit pauses and changes nothing. Run `wattbit doctor`: it lists
any it finds, with the exact command that removes each one.

## Setting a limit

```sh
sudo wattbit limit 80              # stop at 80% (resumes below 75%)
sudo wattbit limit 85              # stop at 85%
sudo wattbit limit disable         # charge normally again (to 100%)
```

The limit percentage can be 50–100% (or `disable`/`off`). In legacy SMC modes,
charging resumes 5% below the limit automatically with built-in hysteresis.
In native MCL mode (macOS 15+), macOS holds the ceiling on AC power with zero
battery cycling. The limit is saved and survives restarts.

## Reading `wattbit status`

```text
$ wattbit status
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

| Line | Meaning |
| --- | --- |
| **Allow charging** | ✔ the battery may charge now; ✘ wattbit is holding it at the limit |
| **Use power adapter** | ✔ the Mac uses the charger; ✘ it runs from the battery |
| **Paused: …** | Another battery app is running; remove it first |
| **Last error: …** | The last hardware problem wattbit hit |
| **Full capacity** | What the battery holds now versus when new; the percentage is its health |
| **Charge rate** | Positive while charging, negative while on battery |
| **Charge control mode** | How the limit is enforced; see below |

| Charge control mode | Meaning |
| --- | --- |
| `native` | Legacy SMC charge-inhibit keys (`CH0B`/`CH0C`/`CHTE`) |
| `native limit (macOS MCL)` | Native macOS Manual Charge Limit via PowerUI (Apple Silicon); powers from charger, zero battery cycling |
| `firmware limit (BCLM)` | Intel firmware enforces the upper limit itself |
| `adapter fallback` | wattbit switches the charger off at the limit |
| `unsupported` | This Mac can't hold a limit; see the next section |

`wattbit status --json` prints the full report for scripts.

## If your Mac can't stop charging

On modern Apple Silicon Macs running macOS 15+ (Sequoia), Apple deprecated
legacy raw SMC keys (`CH0B`, `CH0C`, `CHTE`) and introduced native Manual Charge Limit
(MCL) through the `PowerUI` subsystem. wattbit automatically detects and uses
`native limit (macOS MCL)` mode on these Macs:

- **Zero battery cycling**: When the limit (e.g. 80%) is reached, the system draws
  all power directly from the charger (`Now drawing from 'AC Power'`) while battery
  charging is paused (`IPDChargingAllowed = 0`). The battery does not drain or cycle.
- **Discrete native limits**: Supported native limits are 80%, 85%, 90%, 95%, and 100%.
  If a custom percentage is specified, wattbit snaps it to the closest supported native threshold.

On older Macs lacking both SMC inhibit keys and MCL support, wattbit offers
**adapter fallback**:

```sh
sudo wattbit set adapter-fallback on
sudo wattbit limit 80
```

At 80% the charger is switched off and the Mac runs from its battery while
still plugged in. Below 75% the charger comes back on. The battery cycles
between the two limits, which adds a little wear. That's why the fallback
is off by default.

## Settings

Change a setting with `sudo wattbit set SETTING on|off`.

| Setting | Default | What it does |
| --- | --- | --- |
| `prevent-idle-sleep` | on | Keeps the Mac awake while it charges toward the limit, so wattbit can stop it on time |
| `disable-charging-before-sleep` | on | Stops charging before the Mac sleeps, because wattbit can't act during sleep |
| `prevent-system-sleep` | off | A stronger `prevent-idle-sleep`, like `caffeinate -s`; macOS only honours it on AC power |
| `prevent-sleep-adapter-disabled` | off | Keeps the Mac awake while the charger is switched off |
| `adapter-fallback` | off | Holds the limit by switching the charger off; see [above](#if-your-mac-cant-stop-charging) |
| `magsafe-led` | off | MagSafe light shows green while the limit holds and orange while charging |
| `allow-non-root` | on | Lets any user run `wattbit status` and `wattbit doctor` |
| `discharge-to-limit` | on | Automatically discharges on battery down to the limit when current charge is above it |

## Running on battery while plugged in

```sh
sudo wattbit adapter disable   # run from the battery, charger stays plugged in
sudo wattbit adapter enable    # use the charger again
```

If the battery falls to 10% while the charger is off, wattbit switches it
back on by itself, and it refuses to switch it off at or below 10%.

## Reading `wattbit doctor`

Each line starts with ✔ (working), • (information), ! (worth knowing) or
✘ (fix this). Common lines:

| You see | What to do |
| --- | --- |
| `✘ Conflict: <app>: running` | Run the removal command printed below it |
| `! Conflict: <app>: installed but not running` | Remove it if you no longer use it |
| `✘ Daemon socket: nothing is listening` | Run the [install command](../README.md#install) again |
| `! Version: … is available` | Run the update command it prints |
| `! Daemon version: …` | An update didn't finish; run the install command again |
| `! Charge inhibit: unsupported` | See [If your Mac can't stop charging](#if-your-mac-cant-stop-charging) |
| `! Firmware limit (BCLM): unsupported` | Normal on Apple Silicon; nothing to do |
| `! MagSafe LED control: unverified` | Nothing; it's tested the next time macOS controls the light |
| `• SMC key …: absent` | That key doesn't exist on your Mac; nothing to do |
| `• SMC key CH0J` / `CHLS`: refuses access even as root | macOS 27 reserves these for Apple's own software; nothing to do. wattbit never writes them |

## How it works

The battery service (`wattbitd`) runs as root from boot and is the only part
of wattbit that touches the hardware, through the System Management
Controller (SMC). It reads the battery whenever macOS reports a change, and
every 30 seconds as a backup, then switches charging on or off to hold your
limit.

wattbit picks the best control your Mac's firmware offers when it starts,
and tests it before using it:

| Mode | Used on |
| --- | --- |
| `native` | Apple Silicon and T2 Intel Macs that expose legacy charge-inhibit SMC keys |
| `native limit (macOS MCL)` | Modern Apple Silicon Macs (macOS 15+) via PowerUI (powers from charger, zero battery cycling) |
| `firmware limit (BCLM)` | Intel Macs whose firmware has a charge ceiling |
| `adapter fallback` | Any Mac with charger control, once you turn it on |

Safeguards:

- **Normal charging comes back when wattbit stops,** for an update, an
  uninstall or a shutdown. Uninstalling also resets the hardware itself,
  in case the service had crashed: charging allowed, the charger in use,
  the native and firmware limits off, and the MagSafe LED back to normal.
- **A 10% safety floor** for the charger, as described above.
- **No guessing.** wattbit only writes hardware values it recognizes, and
  reads every change back to confirm it worked.
- **No fighting.** If another battery app is running, wattbit pauses.
- **No hanging.** Every hardware call and connection has a time limit.
- **Safe settings.** `/etc/wattbit.json` is replaced atomically and
  validated when loaded.
- **Only root can change anything.** Every request is checked against the
  caller's user ID; other users can only read status.

## Files

| Path | What it is |
| --- | --- |
| `/Library/PrivilegedHelperTools/wattbitd` | The program |
| `/usr/local/bin/wattbit` | Link that puts `wattbit` on your PATH |
| `/Library/LaunchDaemons/com.wattbit.daemon.plist` | Starts the battery service at boot |
| `/etc/wattbit.json` | Your settings |
| `/var/log/wattbit.log` | The battery service's log |
| `/var/run/wattbit.sock` | How the `wattbit` command talks to the service |

## Troubleshooting

**`other charge limiters are controlling the same hardware`**
Another battery app is running. Remove it with the command wattbit prints;
wattbit resumes by itself once it's gone.

**The battery charged past my limit**
Look for a **Paused** or **Last error** line in `wattbit status`. wattbit
can't act while the Mac sleeps, which is why `disable-charging-before-sleep`
is on by default.

**`requires root; re-run with sudo`**
Commands that change anything need `sudo`, for example
`sudo wattbit limit 80`.

**Watching what wattbit does**
`tail -f /var/log/wattbit.log` shows every decision as it happens: limit
reached, charging stopped, charger back on.

**I used wattbit when it was called berry**
The installer moves your settings and usage history over and removes berry
automatically. `wattbit doctor` lists any berry files that are left.
