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
sudo wattbit limit 80              # stop at 80%, resume below 75%
sudo wattbit limit 85 --lower 60   # stop at 85%, resume below 60%
sudo wattbit limit disable         # charge normally again
```

The upper limit can be 50–100%. Unless you set `--lower`, charging resumes
5% below the limit. The gap stops charging from switching on and off at
every 1% change. The limit is saved and survives restarts.

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
| `native` | The hardware stops charging at the limit |
| `firmware limit (BCLM)` | Intel firmware enforces the upper limit itself |
| `adapter fallback` | wattbit switches the charger off at the limit |
| `unsupported` | This Mac can't hold a limit; see the next section |

`wattbit status --json` prints the full report for scripts.

## If your Mac can't stop charging

On macOS 27, Apple removed the charge-control switch that charge-limit apps
use, at least on the Macs tested so far. Then `sudo wattbit limit 80`
replies *"this Mac exposes no usable charge-inhibit control"*. Choose **one**
of these:

- **Apple's built-in limit (recommended).** macOS 26.4 and later have one in
  **System Settings → Battery → Charging**. Leave wattbit's limit disabled.
- **wattbit's adapter fallback.** wattbit holds the limit by switching the
  charger off and on:

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
| `native` | Apple Silicon and T2 Intel Macs that expose a charge-inhibit control |
| `firmware limit (BCLM)` | Intel Macs whose firmware has a charge ceiling |
| `adapter fallback` | Any Mac with charger control, once you turn it on |

Safeguards:

- **Normal charging comes back when wattbit stops,** for an update, an
  uninstall or a shutdown.
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
