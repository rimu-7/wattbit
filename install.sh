#!/bin/sh
# wattbit installer and updater.
#
#   curl -fsSL https://raw.githubusercontent.com/rimu-7/wattbit/main/install.sh | bash
#
# Downloads the latest wattbit release, checks its SHA-256 checksum,
# installs the battery service and turns on the menu bar speed meter.
# Running it again updates wattbit in place: the service and the meter are
# restarted on the new version, and every setting and the usage history are
# kept. An older install under wattbit's previous name, berry, is moved
# over and removed.
#
# Options (when piping, pass them after `bash -s --`):
#   --version X.Y.Z  install that release instead of the latest
#   --no-speed       leave the speed meter as it is
#   --from-source    build the main branch with cargo instead of downloading
#   --uninstall      remove wattbit and restore factory charging; settings
#                    and usage history are kept
#   --purge          with --uninstall, delete settings and history too,
#                    leaving nothing of wattbit behind
#   --help           show this help
#
# WATTBIT_VERSION and WATTBIT_NO_SPEED=1 work like --version and --no-speed.

set -eu

REPO="rimu-7/wattbit"
ASSET="wattbit-macos-universal.tar.gz"
INSTALLED="/Library/PrivilegedHelperTools/wattbitd"
LINK="/usr/local/bin/wattbit"
# Overridable for mirrors and for testing this script.
RELEASES="${WATTBIT_RELEASES_URL:-https://github.com/$REPO/releases}"
SOURCE="${WATTBIT_SOURCE_URL:-https://github.com/$REPO/archive/refs/heads/main.tar.gz}"

if [ -t 1 ]; then
  BOLD="$(printf '\033[1m')"
  DIM="$(printf '\033[2m')"
  RED="$(printf '\033[31m')"
  YELLOW="$(printf '\033[33m')"
  RESET="$(printf '\033[0m')"
else
  BOLD="" DIM="" RED="" YELLOW="" RESET=""
fi

say() { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
warn() { printf '%swarning:%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() {
  printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2
  exit 1
}

usage() {
  cat <<EOF
Install or update wattbit:

  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | bash

Pass options after \`bash -s --\`, for example \`... | bash -s -- --no-speed\`:

  --version X.Y.Z  install that release instead of the latest
  --no-speed       leave the speed meter as it is
  --from-source    build the main branch with cargo instead of downloading
  --uninstall      remove wattbit and restore factory charging; settings
                   and usage history are kept
  --purge          with --uninstall, delete settings and history too
  --help           show this help
EOF
}

download() {
  # $1: URL, $2: destination
  case "$1" in
    https://*) curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
      --retry 3 --connect-timeout 15 --output "$2" "$1" ;;
    *) curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 \
      --output "$2" "$1" ;;
  esac
}

# Downloads the release archive and its checksum into $WORK and unpacks the
# binary to $WORK/wattbit. Returns non-zero when the release is missing.
fetch_release() {
  if [ -n "$VERSION" ]; then
    base="$RELEASES/download/v${VERSION#v}"
  else
    base="$RELEASES/latest/download"
  fi
  step "Downloading ${VERSION:+wattbit ${VERSION#v} }from $base"
  download "$base/$ASSET" "$WORK/$ASSET" || return 1
  download "$base/$ASSET.sha256" "$WORK/$ASSET.sha256" ||
    die "the release has no checksum file ($ASSET.sha256); refusing to install it."

  expected="$(awk '{ print $1; exit }' "$WORK/$ASSET.sha256")"
  actual="$(shasum -a 256 "$WORK/$ASSET" | awk '{ print $1 }')"
  [ -n "$expected" ] && [ "$expected" = "$actual" ] ||
    die "checksum mismatch for $ASSET (expected $expected, got $actual). Nothing was installed."
  say "${DIM}    checksum verified: $actual${RESET}"

  tar -xzf "$WORK/$ASSET" -C "$WORK" wattbit ||
    die "the release archive does not contain the wattbit binary."
  BIN="$WORK/wattbit"
}

build_from_source() {
  command -v cargo >/dev/null 2>&1 ||
    die "building from source needs Rust. Install it from https://rustup.rs and run this again."
  step "Building wattbit from source (this takes a minute)"
  download "$SOURCE" "$WORK/source.tar.gz" || die "cannot download the source from $SOURCE"
  mkdir "$WORK/source"
  tar -xzf "$WORK/source.tar.gz" -C "$WORK/source" --strip-components 1
  (cd "$WORK/source" && cargo build --release --locked --quiet) </dev/null ||
    die "the build failed; see the output above."
  BIN="$WORK/source/target/release/wattbit"
}

# The installed version, or nothing when wattbit is not installed.
installed_version() {
  if [ -x "$INSTALLED" ]; then
    "$INSTALLED" --version 2>/dev/null | awk '{ print $2 }'
  fi
}

make_work_dir() {
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/wattbit-install.XXXXXX")"
  trap 'rm -rf "$WORK"' EXIT
  trap 'exit 130' INT TERM
}

# Makes $BIN runnable on this Mac and sets NEW to its version. Downloads
# are ad-hoc signed; make sure the copy runs on Apple Silicon and is not
# held back by a quarantine flag.
prepare_binary() {
  xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
  codesign --verify "$BIN" 2>/dev/null || codesign --force --sign - "$BIN" 2>/dev/null || true
  NEW="$("$BIN" --version 2>/dev/null | awk '{ print $2 }')" || return 1
  [ -n "$NEW" ]
}

# Prints, one per line, what wattbit (or berry, its old name) still has on
# this Mac for the current user. Settings and usage history only count
# with --purge.
leftovers() {
  for path in \
    /Library/LaunchDaemons/com.wattbit.daemon.plist "$INSTALLED" \
    /var/run/wattbit.sock /var/log/wattbit.log \
    /Library/LaunchDaemons/com.berry.daemon.plist /Library/PrivilegedHelperTools/berryd \
    /var/run/berry.sock /var/log/berry.log \
    "$HOME/Library/LaunchAgents/com.wattbit.speed.plist" \
    "$HOME/Library/LaunchAgents/com.berry.speed.plist"; do
    if [ -e "$path" ] || [ -L "$path" ]; then say "$path"; fi
  done
  for link in "$LINK" /usr/local/bin/berry; do
    if [ -L "$link" ]; then
      case "$(readlink "$link")" in */wattbitd | */berryd) say "$link" ;; esac
    fi
  done
  if [ "$PURGE" = 1 ]; then
    for path in /etc/wattbit.json /etc/berry.json \
      "$HOME/Library/Application Support/wattbit" \
      "$HOME/Library/Application Support/berry"; do
      if [ -e "$path" ] || [ -L "$path" ]; then say "$path"; fi
    done
  fi
  for job in system/com.wattbit.daemon system/com.berry.daemon \
    "gui/$(id -u)/com.wattbit.speed" "gui/$(id -u)/com.berry.speed"; do
    if launchctl print "$job" >/dev/null 2>&1; then say "launchd job $job"; fi
  done
  return 0
}

# Removes wattbit's files without wattbit's help, for when its binary is
# gone, or is a release that predates part of today's cleanup. It cannot
# check the charging hardware; only `wattbit uninstall-service` can.
remove_by_hand() {
  uid="$(id -u)"
  for label in com.wattbit.speed com.berry.speed; do
    launchctl bootout "gui/$uid/$label" 2>/dev/null || true
    plist="$HOME/Library/LaunchAgents/$label.plist"
    if [ -f "$plist" ] && grep -q "<string>$label</string>" "$plist"; then
      rm -f "$plist"
    fi
  done
  # A meter started with --no-launchd has no login item.
  pkill -U "$uid" -x wattbitd 2>/dev/null || true
  pkill -U "$uid" -x berryd 2>/dev/null || true
  data="$HOME/Library/Application Support"
  if [ "$PURGE" = 1 ]; then
    rm -rf "$data/wattbit"
    # berry's directory: only the files berry is known to have written.
    if [ -d "$data/berry" ]; then
      rm -f "$data/berry/speed.json" "$data/berry/speed.pid" "$data/berry/speed.sock" \
        "$data/berry/speed.log" "$data/berry/update.json" \
        "$data/berry"/speed.json.corrupt.* "$data/berry"/.speed.json.tmp.* \
        "$data/berry"/.update.json.tmp.*
      rmdir "$data/berry" 2>/dev/null || warn "left $data/berry in place: it holds files berry did not create."
    fi
  else
    rm -f "$data/wattbit/speed.pid" "$data/wattbit/speed.sock" \
      "$data/berry/speed.pid" "$data/berry/speed.sock"
  fi

  sudo /bin/sh -s "$PURGE" <<'EOF' || true
purge="$1"
for label in com.wattbit.daemon com.berry.daemon; do
  /bin/launchctl bootout "system/$label" 2>/dev/null
done
rm -f /Library/LaunchDaemons/com.wattbit.daemon.plist /Library/LaunchDaemons/com.berry.daemon.plist \
  /Library/PrivilegedHelperTools/wattbitd /Library/PrivilegedHelperTools/berryd \
  /var/log/wattbit.log /var/log/berry.log \
  /Library/PrivilegedHelperTools/.wattbitd.tmp.* /Library/LaunchDaemons/.com.wattbit.daemon.plist.tmp.*
for socket in /var/run/wattbit.sock /var/run/berry.sock; do
  if [ -S "$socket" ]; then rm -f "$socket"; fi
done
for link in /usr/local/bin/wattbit /usr/local/bin/berry; do
  if [ -L "$link" ]; then
    case "$(readlink "$link")" in */wattbitd | */berryd) rm -f "$link" ;; esac
  fi
done
if [ "$purge" = 1 ]; then
  rm -f /etc/wattbit.json /etc/berry.json /etc/.wattbit.json.tmp.*
fi
EOF
}

uninstall() {
  if [ -z "$(leftovers)" ]; then
    say "wattbit is not installed; nothing to do."
    return
  fi

  BIN=""
  if [ -x "$INSTALLED" ]; then
    BIN="$INSTALLED"
  else
    # Parts of wattbit are left but its binary is not. Only wattbit can put
    # the charging hardware back, so fetch a copy to finish the job.
    step "wattbit's binary is missing; downloading a copy to finish the uninstall"
    make_work_dir
    if fetch_release && prepare_binary; then
      :
    else
      BIN=""
      warn "could not get wattbit; its files are removed by hand instead."
    fi
  fi

  if [ -n "$BIN" ]; then
    step "Stopping the speed meter"
    "$BIN" speed off </dev/null >/dev/null 2>&1 || true
    step "Restoring factory charging and removing wattbit (administrator access needed)"
    if [ "$PURGE" = 1 ]; then
      sudo "$BIN" uninstall-service --purge </dev/null || warn "wattbit could not finish; cleaning up by hand."
    else
      sudo "$BIN" uninstall-service </dev/null || warn "wattbit could not finish; cleaning up by hand."
    fi
  else
    step "Removing wattbit (administrator access needed)"
  fi
  # Catches what an older wattbit did not remove; a no-op otherwise.
  if [ -n "$(leftovers)" ]; then
    remove_by_hand
  fi

  remaining="$(leftovers)"
  if [ -n "$remaining" ]; then
    warn "these are still on this Mac:"
    printf '%s\n' "$remaining" | sed 's/^/    /' >&2
    die "remove them by hand, or report it at https://github.com/$REPO/issues"
  fi
  say ""
  say "${BOLD}wattbit is uninstalled.${RESET} Normal charging is restored."
  if [ -z "$BIN" ]; then
    say "wattbit could not check the charging hardware; restart your Mac to be sure charging is back to normal."
  fi
  if [ "$PURGE" = 1 ]; then
    say "Your settings and usage history are deleted too; nothing of wattbit is left."
  else
    say "Your settings (/etc/wattbit.json) and usage history are kept; run this again with --purge to delete them."
  fi
}

main() {
  VERSION="${WATTBIT_VERSION:-}"
  NO_SPEED="${WATTBIT_NO_SPEED:-0}"
  FROM_SOURCE=0
  UNINSTALL=0
  PURGE=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --version)
        [ $# -ge 2 ] || die "--version needs a value, for example --version 0.3.0"
        VERSION="$2"
        shift
        ;;
      --version=*) VERSION="${1#--version=}" ;;
      --no-speed) NO_SPEED=1 ;;
      --from-source) FROM_SOURCE=1 ;;
      --uninstall) UNINSTALL=1 ;;
      --purge) PURGE=1 ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
  done

  [ "$(uname -s)" = Darwin ] || die "wattbit runs on macOS only."
  OS_VERSION="$(sw_vers -productVersion 2>/dev/null || echo unknown)"
  OS_MAJOR="${OS_VERSION%%.*}"
  # Big Sur reports 10.16 to programs that ask for compatibility.
  case "$OS_VERSION" in 10.16*) OS_MAJOR=11 ;; esac
  case "$OS_MAJOR" in
    '' | *[!0-9]*) die "cannot tell which macOS this is ($OS_VERSION)." ;;
  esac
  [ "$OS_MAJOR" -ge 11 ] ||
    die "wattbit needs macOS 11 Big Sur or later; this Mac runs macOS $OS_VERSION."
  [ "$(id -u)" -ne 0 ] ||
    die "run this as yourself, without sudo. It asks for your password when it needs it."
  for tool in curl tar shasum awk; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is missing."
  done

  if [ "$UNINSTALL" = 1 ]; then
    uninstall
    return
  fi
  [ "$PURGE" = 0 ] || die "--purge only goes with --uninstall."

  make_work_dir

  if [ "$FROM_SOURCE" = 1 ]; then
    build_from_source
  elif ! fetch_release; then
    if [ -z "$VERSION" ] && command -v cargo >/dev/null 2>&1; then
      warn "no release is published yet; building from source instead."
      build_from_source
    elif [ -n "$VERSION" ]; then
      die "wattbit ${VERSION#v} was not found at $RELEASES."
    else
      die "no wattbit release is published yet. Install Rust from https://rustup.rs and run this again to build from source."
    fi
  fi

  prepare_binary || die "the downloaded binary does not run on this Mac ($(uname -m))."

  OLD="$(installed_version)"
  if [ -z "$OLD" ]; then
    step "Installing wattbit $NEW"
  elif [ "$OLD" = "$NEW" ]; then
    step "Reinstalling wattbit $NEW"
  else
    step "Updating wattbit $OLD -> $NEW"
  fi

  say "    The battery service runs as root, so your password is needed once."
  sudo "$BIN" install-service </dev/null ||
    die "installing the battery service failed; see the message above."

  # Detect controlling terminal for interactive configuration (even when piped via curl | bash)
  TTY=""
  if [ -r /dev/tty ]; then
    TTY="/dev/tty"
  elif [ -t 0 ]; then
    TTY="/dev/stdin"
  fi

  WANT_LIMIT=""
  WANT_SPEED=""

  if [ -n "$TTY" ]; then
    say ""
    step "Configuration"

    printf "  Set battery charge limit percentage [80/85/90/100, or 'disable'] (default: 80): "
    read -r CHOSEN_LIMIT < "$TTY" || CHOSEN_LIMIT=""
    CHOSEN_LIMIT="$(echo "$CHOSEN_LIMIT" | tr -d '[:space:]')"
    case "$CHOSEN_LIMIT" in
      '' | 80 | 80%) WANT_LIMIT="80" ;;
      disable | off | DISABLE | OFF) WANT_LIMIT="disable" ;;
      5[0-9] | 6[0-9] | 7[0-9] | 8[0-9] | 9[0-9] | 100 | *%) WANT_LIMIT="${CHOSEN_LIMIT%\%}" ;;
      *)
        warn "unrecognized limit '$CHOSEN_LIMIT'; defaulting to 80%"
        WANT_LIMIT="80"
        ;;
    esac

    if [ "$NO_SPEED" != 1 ]; then
      printf "  Enable internet speed meter in menu bar? [Y/n] (default: Y): "
      read -r CHOSEN_SPEED < "$TTY" || CHOSEN_SPEED=""
      CHOSEN_SPEED="$(echo "$CHOSEN_SPEED" | tr -d '[:space:]')"
      case "$CHOSEN_SPEED" in
        '' | [yY] | [yY][eE][sS]) WANT_SPEED=1 ;;
        [nN] | [nN][oO] | off | OFF) WANT_SPEED=0 ;;
        *) WANT_SPEED=1 ;;
      esac
    else
      WANT_SPEED=0
    fi
  else
    # Non-interactive mode (CI or piped without tty)
    if [ -z "$OLD" ]; then
      WANT_LIMIT="80"
      WANT_SPEED=$([ "$NO_SPEED" = 1 ] && echo 0 || echo 1)
    fi
  fi

  # Apply charge limit if specified
  if [ -n "$WANT_LIMIT" ]; then
    step "Applying charge limit ($WANT_LIMIT)"
    sudo "$INSTALLED" limit "$WANT_LIMIT" </dev/null 2>/dev/null || true
  fi

  # Start or stop speed meter
  if [ "$WANT_SPEED" = 1 ]; then
    step "Starting the speed meter"
    "$INSTALLED" speed on </dev/null ||
      warn "the speed meter did not start. Run \`wattbit speed on\` once you are logged in at the Mac."
  elif [ "$WANT_SPEED" = 0 ] && [ "$NO_SPEED" != 1 ]; then
    "$INSTALLED" speed off </dev/null 2>/dev/null || true
  elif [ "$NO_SPEED" = 1 ]; then
    say "${DIM}    Leaving the speed meter as it is (--no-speed).${RESET}"
  fi

  WATTBIT="wattbit"
  case ":$PATH:" in
    *":$(dirname "$LINK"):"*) ;;
    *) WATTBIT="$INSTALLED" ;;
  esac

  say ""
  if [ -n "$OLD" ] && [ "$OLD" != "$NEW" ]; then
    say "${BOLD}wattbit is updated to $NEW.${RESET} Your settings and usage history are kept."
  else
    say "${BOLD}wattbit $NEW is installed.${RESET}"
  fi
  say ""
  say "${BOLD}Core Commands:${RESET}"
  say "  sudo $WATTBIT limit <PERCENT>   set charge limit (e.g. 80, 85, 90)"
  say "  sudo $WATTBIT limit disable     disable limit (charge to 100%)"
  say "  $WATTBIT status                 charging, battery & settings overview"
  say "  $WATTBIT doctor                 run system & hardware diagnostics"
  say "  $WATTBIT speed on               enable menu bar speed meter"
  say "  $WATTBIT speed off              disable menu bar speed meter"
  say "  $WATTBIT speed status           view speed and 31-day data usage"
  say ""
  say "  Update anytime by re-running the install command."
}

main "$@"
