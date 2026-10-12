#!/usr/bin/env sh
set -eu

# The audit service units are rendered from the checkout's real location with
# each format's escaping, .env settings are honoured without being executed,
# and a macOS install retires an agent that runs this checkout under another label.
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
SOURCE_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/audit-service.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

# A checkout in an arbitrary location, with characters sed, XML, and systemd treat specially.
ROOT="$TMP_ROOT/some where&else%/harness"
mkdir -p "$ROOT/scripts" "$ROOT/ops" "$TMP_ROOT/home"
cp "$SOURCE_ROOT/scripts/audit-service.sh" "$ROOT/scripts/"
cp "$SOURCE_ROOT/ops/harness-audit.service" "$SOURCE_ROOT/ops/harness-audit.launchd.plist" "$ROOT/ops/"
SERVICE="$ROOT/services/harness-audit"
UNIT_ROOT=$(printf '%s' "$ROOT" | sed 's/%/%%/g')

audit() {
  env -u HARNESS_AUDIT_ROOT -u HARNESS_AUDIT_LAUNCHD_LABEL HOME="$TMP_ROOT/home" sh "$ROOT/scripts/audit-service.sh" "$@"
}

# Print LABEL|PROGRAM|WORKDIR|LOG from a plist on stdin; fails when it does not parse.
plist_fields() {
  python3 -c 'import plistlib, sys
p = plistlib.loads(sys.stdin.buffer.read())
print("|".join([p["Label"], p["ProgramArguments"][1], p["WorkingDirectory"], p["StandardOutPath"]]))'
}

unit=$(audit render systemd)
printf '%s\n' "$unit" | grep -qxF "ExecStart=\"$UNIT_ROOT/scripts/audit-service.sh\" serve" || fail "systemd ExecStart is not the quoted, escaped checkout path: $unit"
printf '%s\n' "$unit" | grep -qxF "WorkingDirectory=$UNIT_ROOT/services/harness-audit" || fail "systemd WorkingDirectory is not the escaped service path"
if printf '%s\n' "$unit" | grep -q '__'; then fail "systemd unit has an unfilled placeholder"; fi

fields=$(audit render launchd | plist_fields) || fail "launchd plist does not parse"
[ "$fields" = "local.harness.audit|$ROOT/scripts/audit-service.sh|$SERVICE|$TMP_ROOT/home/Library/Logs/harness-audit.log" ] \
  || fail "launchd plist fields are wrong: $fields"

# .env overrides, in the forms people write; other lines are ignored and nothing is executed.
printf '%s\r\n' "  export HARNESS_AUDIT_LAUNCHD_LABEL=\"com.example.audit\"" > "$ROOT/.env"
cat >> "$ROOT/.env" <<EOF
HARNESS_AUDIT_ROOT='$TMP_ROOT/elsewhere'
PATH=/nonexistent
touch $TMP_ROOT/executed
EOF
fields=$(audit render launchd | plist_fields) || fail "launchd plist with .env settings does not parse"
[ "$fields" = "com.example.audit|$ROOT/scripts/audit-service.sh|$TMP_ROOT/elsewhere|$TMP_ROOT/home/Library/Logs/harness-audit.log" ] \
  || fail ".env settings were not read cleanly: $fields"
[ ! -e "$TMP_ROOT/executed" ] || fail ".env content was executed"

# The environment wins over .env.
fields=$(HARNESS_AUDIT_LAUNCHD_LABEL=com.example.env HOME="$TMP_ROOT/home" sh "$ROOT/scripts/audit-service.sh" render launchd | plist_fields)
case "$fields" in com.example.env\|*) ;; *) fail "environment did not win over .env: $fields" ;; esac
rm -f "$ROOT/.env"

# macOS install: an agent for this checkout under an old label is retired; others are left alone.
BIN="$TMP_ROOT/bin"
mkdir -p "$BIN" "$SERVICE" "$TMP_ROOT/home/Library/LaunchAgents"
: > "$SERVICE/Cargo.toml"
printf '#!/bin/sh\necho Darwin\n' > "$BIN/uname"
printf '#!/bin/sh\necho "$*" >> "%s/launchctl.log"\n' "$TMP_ROOT" > "$BIN/launchctl"
printf '#!/bin/sh\nexit 0\n' > "$BIN/cargo"
chmod +x "$BIN/uname" "$BIN/launchctl" "$BIN/cargo"
HARNESS_AUDIT_LAUNCHD_LABEL=com.example.old HOME="$TMP_ROOT/home" sh "$ROOT/scripts/audit-service.sh" render launchd \
  > "$TMP_ROOT/home/Library/LaunchAgents/com.example.old.plist"
printf '<plist><string>/elsewhere/scripts/audit-service.sh</string></plist>\n' > "$TMP_ROOT/home/Library/LaunchAgents/com.example.other.plist"
PATH="$BIN:$PATH" CARGO="$BIN/cargo" audit install > "$TMP_ROOT/install.out"
[ ! -e "$TMP_ROOT/home/Library/LaunchAgents/com.example.old.plist" ] || fail "install left the old-label agent for this checkout"
[ -e "$TMP_ROOT/home/Library/LaunchAgents/com.example.other.plist" ] || fail "install removed an unrelated agent"
[ -e "$TMP_ROOT/home/Library/LaunchAgents/local.harness.audit.plist" ] || fail "install did not write the new label"
grep -q "bootout gui/[0-9]*/com.example.old" "$TMP_ROOT/launchctl.log" || fail "install did not boot out the old label"
grep -q "Retiring launchd agent com.example.old" "$TMP_ROOT/install.out" || fail "install did not say it retired the old label"

printf '%s\n' 'PASS: audit service units render from the checkout location with format escaping, honour .env, and retire an old label'
