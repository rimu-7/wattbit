# wattbit

**Battery charge limiting and a live internet speed meter for your Mac — in one small, native command-line tool.**

[![Latest release](https://badgen.net/github/release/rimu-7/wattbit?label=release)](https://github.com/rimu-7/wattbit/releases/latest)
[![CI](https://github.com/rimu-7/wattbit/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/rimu-7/wattbit/actions/workflows/ci.yml)
![macOS 11+](https://img.shields.io/badge/macOS-11%2B-000000?logo=apple)
![Apple Silicon and Intel](https://img.shields.io/badge/Apple%20Silicon%20%2B%20Intel-universal-0a84ff)

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
```

One command installs everything. Run it again any time to update.

---

- [Features](#features)
- [Compatibility](#compatibility)
- [Install](#install)
- [Quick start](#quick-start)
- [Updating](#updating)
- [Uninstalling](#uninstalling)
- [Commands](#commands)
- [Troubleshooting](#troubleshooting)
- [Privacy and security](#privacy-and-security)
- [Documentation](#documentation)

## Features

**Battery care.** Keeping a battery at 100% all day wears it out faster.
wattbit stops charging at the limit you choose and keeps your Mac running directly
from the charger (AC pass-through), with zero battery cycling.

- `sudo wattbit limit 80` stops charging at 80% and powers the Mac directly from the cable across restarts.
- **Smart discharge-to-limit:** if your battery is currently above the limit (e.g. 99%), wattbit automatically discharges on battery down to your limit, then switches to charger power.
- **Native Apple Silicon support (MCL):** controls macOS native Manual Charge Limit via PowerUI on modern Macs running macOS 15 (Sequoia) or later.
- Reacts to every change macOS reports, and re-checks every 30 seconds.
- Shows charge, health, cycle count, charge rate and voltage.
- Can run the Mac from battery while plugged in (`wattbit adapter disable`).
- Honest about hardware: if your Mac can't do something, wattbit says so and explains why.

**Internet speed.** Live speeds in your menu bar: `↓ 1.23 MB/s ↑ 340 KB/s`.

- Refreshed every second.
- 31 days of daily download and upload totals, kept on your Mac.
- Tells you when a new wattbit is out, with the command to update.
- No admin rights, extensions or packet capture: it reads the same counters
  as `netstat`.

## Compatibility

One **universal binary** runs natively on both kinds of Mac.

| | Requirement |
| --- | --- |
| **macOS** | 11 Big Sur or later |
| **Processor** | Apple Silicon (M1, M2, M3, M4 and later) or Intel |
| **Account** | An administrator, to install the battery service |

| Feature | Apple Silicon | Intel | Desktop Macs |
| --- | --- | --- | --- |
| Speed meter | ✔ | ✔ | ✔ |
| Battery details | ✔ | ✔ | — no battery |
| Charge limit | ✔ (native MCL & SMC) | ✔ (BCLM firmware) | — |
| AC pass-through (zero cycling) | ✔ (powers from charger) | ✔ | — |
| Run on battery while plugged in | ✔ | ✔ on models that support it | — |

Run `wattbit doctor` to see exactly what your Mac supports.

## Install

Open **Terminal** and run:

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
```

The installer checks your macOS version and downloads the latest release,
refusing it unless the SHA-256 checksum matches. It then asks for your
password **once** to install the battery service, adds `wattbit` to your
PATH, and turns on the speed meter. Run it as yourself, not with `sudo`.

Options go after `bash -s --`, for example `... | bash -s -- --no-speed`:

| Option | What it does |
| --- | --- |
| `--no-speed` | Don't turn on the speed meter |
| `--version 0.5.0` | Install a specific release |
| `--from-source` | Build from source with Rust instead of downloading |
| `--uninstall [--purge]` | Remove wattbit (`--purge` also deletes settings and history) |

## Quick start

```sh
wattbit doctor           # 1. check your Mac; fix anything marked ✘
sudo wattbit limit 80    # 2. stop charging at 80%
wattbit status           # 3. confirm the limit is holding
wattbit speed status     # 4. see your speeds and usage
```

Another battery app (AlDente, batt, Battery Toolkit, `battery`) must be
removed first: only one app can control charging. `wattbit doctor` shows the
command to remove each one.

## Updating

The speed meter checks for new releases every 12 hours. When one is out, its
menu shows **⚠️ Update available**, the command to run and **Copy Update
Command**. Paste the command into Terminal. It is the install command:

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
```

Updating restarts the battery service and the speed meter on the new
version, and keeps your charge limit, settings and usage history.

## Uninstalling

```sh
curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash -s -- --uninstall
```

Normal charging is restored immediately. Add `--purge` to also delete your
settings and usage history.

## Commands

| Command | What it does | `sudo` |
| --- | --- | --- |
| `wattbit status` | Charging, battery, settings and hardware support | |
| `wattbit doctor` | Checks everything and explains how to fix problems | |
| `sudo wattbit limit <PERCENT>` | Sets the charge limit (e.g. `80`, `85`, `90`) | ✔ |
| `sudo wattbit limit disable` | Charges normally again (to 100%) | ✔ |
| `sudo wattbit adapter disable` / `enable` | Runs from battery while plugged in / uses the charger | ✔ |
| `sudo wattbit set SETTING on\|off` | Changes a [setting](docs/BATTERY.md#settings) | ✔ |
| `wattbit speed on` / `off` | Shows / hides the speed meter in menu bar | |
| `wattbit speed status` | Live speeds and 31 days of usage | |

`status` and `speed status` accept `--json`. Every command explains itself
with `--help`.

## Troubleshooting

Start with **`wattbit doctor`**: problems are marked ✘ and come with the
command that fixes them.

- **`command not found: wattbit`** or **`the wattbit daemon is not running`:**
  run the install command again.
- **The battery charged past the limit:** check `wattbit status` for a
  **Paused** or **Last error** line.
- **The speed meter isn't in the menu bar:** the menu bar may be full; see
  the [speed meter guide](docs/SPEED.md#troubleshooting).

More answers are in the [charge limiting guide](docs/BATTERY.md#troubleshooting).

## Privacy and security

- **Your data stays on your Mac.** Usage history is stored in your Library
  folder, readable only by you.
- **One small network request, twice a day:** the update check asks GitHub
  for the latest version number. Nothing about your Mac is sent. To turn it
  off: `WATTBIT_NO_UPDATE_CHECK=1 wattbit speed on`.
- **Least privilege.** Only the battery service runs as root, and it never
  touches the network. Only `sudo` can change its settings.
- **Safe by design.** Normal charging returns whenever wattbit stops, and
  every hardware change is read back to confirm it worked.

## Documentation

| Guide | What's in it |
| --- | --- |
| [Charge limiting](docs/BATTERY.md) | Limits, reading `status` and `doctor`, settings, macOS 27, safeguards, files |
| [Speed meter](docs/SPEED.md) | The menu, usage history, what is counted, data files, troubleshooting |
| [Development](docs/DEVELOPMENT.md) | Building, architecture, testing, compatibility checks, releasing |

Bug reports and pull requests are welcome at
[github.com/rimu-7/wattbit](https://github.com/rimu-7/wattbit/issues).

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=rimu-7/wattbit&type=Date)](https://star-history.com/#rimu-7/wattbit&Date)
