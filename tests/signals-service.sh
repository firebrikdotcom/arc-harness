#!/usr/bin/env sh
set -eu

# harness-signals: the crate is formatted, lint-clean and its tests pass (config,
# adapters, card mapping, scoring, webhook signatures, an end-to-end poll against a
# fake Sentry), and its service units render this checkout's real locations.
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CRATE="$ROOT/services/harness-signals/Cargo.toml"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

CARGO_BIN=$(command -v cargo || true)
[ -n "$CARGO_BIN" ] || CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -x "$CARGO_BIN" ] || fail "cargo is required to test harness-signals"

"$CARGO_BIN" fmt --manifest-path "$CRATE" -- --check || fail "harness-signals is not formatted (cargo fmt)"
"$CARGO_BIN" clippy --quiet --manifest-path "$CRATE" --all-targets -- -D warnings || fail "clippy found problems in harness-signals"
"$CARGO_BIN" test --quiet --manifest-path "$CRATE" || fail "harness-signals tests failed"

systemd=$(sh "$ROOT/scripts/signals-service.sh" render systemd)
printf '%s\n' "$systemd" | grep -qF "ExecStart=\"$ROOT/scripts/signals-service.sh\" serve" || fail "systemd unit does not run this checkout"
printf '%s\n' "$systemd" | grep -qF "WorkingDirectory=$ROOT/services/harness-signals" || fail "systemd unit has the wrong working directory"
launchd=$(HOME=/tmp/signals-home sh "$ROOT/scripts/signals-service.sh" render launchd)
printf '%s\n' "$launchd" | grep -qF "<string>$ROOT/scripts/signals-service.sh</string>" || fail "launchd plist does not run this checkout"
printf '%s\n' "$launchd" | grep -qF "<string>/tmp/signals-home/Library/Logs/harness-signals.log</string>" || fail "launchd plist logs to the wrong place"
printf '%s\n' "$launchd" | grep -qF "<string>local.harness.signals</string>" || fail "launchd plist has the wrong label"

printf '%s\n' 'PASS: harness-signals config, adapters, mapping, scoring, webhooks, and service units'
