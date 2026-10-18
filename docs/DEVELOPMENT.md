# Developing wattbit

Everything needed to build, test and release wattbit. For using it, see the
[README](../README.md); for the speed meter's internals, see
[SPEED.md](SPEED.md).

- [Building](#building)
- [Layout](#layout)
- [Architecture](#architecture)
- [The charge-limit state machine](#the-charge-limit-state-machine)
- [Daemon protocol](#daemon-protocol)
- [Testing](#testing)
- [Compatibility](#compatibility)
- [Releasing](#releasing)

## Building

You need macOS 11 or later and Rust from [rustup.rs](https://rustup.rs).

```sh
cargo build --release
./target/release/wattbit --version
```

To install a local build exactly as users get it:

```sh
sudo ./target/release/wattbit install-service
wattbit speed on
```

[`.cargo/config.toml`](../.cargo/config.toml) sets
`MACOSX_DEPLOYMENT_TARGET=11.0`, so every build, local or CI, targets the
same minimum macOS.

## Layout

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
├── speed/             internet speed meter (see SPEED.md)
└── ffi.rs             IOKit declarations taken from the macOS SDK headers
install.sh             the one-line installer and updater
```

## Architecture

wattbit is one binary with two roles:

- **`wattbitd`**, the root LaunchDaemon (`com.wattbit.daemon`), is the only
  part that touches the SMC. It serves requests on `/var/run/wattbit.sock`,
  re-evaluates the policy on every power-source notification and every
  30 seconds, and restores factory charging when it stops.
- **The speed agent** (`com.wattbit.speed`) is a per-user LaunchAgent that
  owns the menu bar item. It never needs root and never talks to the daemon.

wattbit uses only native frameworks (IOKit, CoreFoundation and AppKit),
reached through hand-written declarations. It runs two external programs:
`/bin/launchctl` to load and unload its services (macOS has no supported C
API that lets a command-line tool load a system service), and
`/usr/bin/curl` for the speed agent's release check, so it needs no TLS
library of its own. The root daemon never touches the network.

SMC control values are stored as exact bytes rather than numbers, because
SMC integers are little-endian on Apple Silicon but big-endian on Intel.

## The charge-limit state machine

With an upper limit *U* and a lower limit *L*:

| State | When | Charging |
| --- | --- | --- |
| `ChargingPermitted` | charge is below *L*, or between the limits while charging up | allowed |
| `ChargingSuppressed` | charge has reached *U* | stopped |
| `WaitingToResumeCharging` | charge is between the limits after reaching *U* | stopped |

The gap between the limits keeps charging from toggling at every 1%
change. Every hardware write is read back to confirm it took effect.

## Daemon protocol

The `wattbit` command sends one JSON-RPC 2.0 request per connection to
`/var/run/wattbit.sock`, as a single line:

```json
{"jsonrpc":"2.0","id":1,"method":"set_limit","params":{"upper":80,"lower":75}}
{"jsonrpc":"2.0","id":1,"result":{"kind":"ack","message":"Charge limit set: ..."}}
```

Methods: `status`, `doctor`, `set_limit`, `disable_limit`, `set_adapter`,
`set_option`. Besides the standard JSON-RPC errors, wattbit uses `-32001`
(unauthorized), `-32002` (unsupported), `-32003` (hardware failure), `-32004`
(timeout) and `-32005` (another battery app running). `error.data.kind`
names the category. Every request is authorized with the caller's uid
(`getpeereid`); only root may change anything.

## Testing

```sh
cargo test                                            # all automated tests
cargo test --test speed                               # speed meter end to end
cargo test --test hardware -- --ignored --nocapture   # read-only checks of this Mac
cargo test --lib copies_to_the_clipboard -- --ignored # menu's clipboard action
```

To try changes without installing, run the daemon in read-only mode. It
doesn't need root, reads the hardware but never changes it, and is what the
end-to-end tests use:

```sh
cargo run -- daemon run --read-only --socket /tmp/wattbit.sock --config /tmp/wattbit.json
cargo run -- status --socket /tmp/wattbit.sock
```

The end-to-end tests run the speed agent with a private `WATTBIT_SPEED_DIR`
and `HOME`, `WATTBIT_SPEED_NO_GUI=1` and `WATTBIT_NO_UPDATE_CHECK=1`, so they
never create a menu bar item, never touch `~/Library` and never reach the
network.

## Compatibility

The release is one universal binary (arm64 + x86_64) that targets
**macOS 11 Big Sur or later**. `install.sh` refuses older systems before
downloading anything.

[CI](../.github/workflows/ci.yml) runs the whole suite natively on every
macOS runner GitHub offers, Apple Silicon and Intel, and smoke-tests the
release build on each. After every release, the
[release workflow](../.github/workflows/release.yml) downloads the published
archive and runs it on an Apple Silicon and an Intel runner.

## Releasing

1. Bump `version` in `Cargo.toml` and run `cargo build` so `Cargo.lock`
   follows.
2. Commit, then tag and push:

   ```sh
   git tag v0.2.1
   git push origin main v0.2.1
   ```

The release workflow checks that the tag matches `Cargo.toml`, runs the
tests, builds the universal binary, signs it ad hoc and publishes
`wattbit-macos-universal.tar.gz` with its `.sha256` file. It then verifies
the download on both chips. From then on, `install.sh` installs that
release, and every running speed meter offers the update within 12 hours.
