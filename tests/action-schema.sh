#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd -P)
ACTION="$HARNESS_ROOT/scripts/action.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-action.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

expect_pass() {
  name="$1"
  file="$2"
  if ! "$ACTION" validate "$file" >/dev/null; then
    fail "$name unexpectedly rejected"
  fi
}

expect_fail() {
  name="$1"
  file="$2"
  if "$ACTION" validate "$file" >/dev/null 2>&1; then
    fail "$name unexpectedly passed"
  fi
}

printf '%s\n' '{"version":1,"type":"run_command","command":["scripts/verify.sh"]}' > "$TMP_ROOT/valid-run.json"
printf '%s\n' '{"version":1,"type":"write_file","path":"docs/setup.md","reason":"document the validator"}' > "$TMP_ROOT/valid-write.json"
printf '%s\n' '{"version":1,"type":"delete_file","path":"docs/setup.md"}' > "$TMP_ROOT/unknown-type.json"
printf '%s\n' '{"version":1,"type":"run_command","command":["scripts/verify.sh"],"extra":true}' > "$TMP_ROOT/extra-field.json"
printf '%s\n' '{"version":1,"type":"run_command","command":"scripts/verify.sh"}' > "$TMP_ROOT/command-string.json"
printf '%s\n' '{"type":"run_command","command":["scripts/verify.sh"]}' > "$TMP_ROOT/missing-version.json"
printf '%s\n' '{"version":1,"type":"run_command","command":' > "$TMP_ROOT/invalid.json"

expect_pass "valid run_command" "$TMP_ROOT/valid-run.json"
expect_pass "valid write_file" "$TMP_ROOT/valid-write.json"
expect_fail "unknown type" "$TMP_ROOT/unknown-type.json"
expect_fail "extra field" "$TMP_ROOT/extra-field.json"
expect_fail "command as string" "$TMP_ROOT/command-string.json"
expect_fail "missing version" "$TMP_ROOT/missing-version.json"
expect_fail "invalid JSON" "$TMP_ROOT/invalid.json"

printf '%s\n' 'PASS: action schema validation'
