#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd -P)
VERIFY="$HARNESS_ROOT/scripts/verify.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-verify.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

pass_project="$TMP_ROOT/pass"
missing_project="$TMP_ROOT/missing"
override_project="$TMP_ROOT/override"
mkdir -p "$pass_project/scripts" "$missing_project" "$override_project/scripts"
printf '%s\n' '#!/usr/bin/env sh' 'exit 0' > "$pass_project/scripts/check.sh"
printf '%s\n' 'test' > "$pass_project/.harness-required-checks"
printf '%s\n' 'lint' > "$missing_project/.harness-required-checks"
printf '%s\n' '#!/usr/bin/env sh' 'exit 0' > "$override_project/scripts/check.sh"
printf '%s\n' 'lint' > "$override_project/.harness-required-checks"

"$VERIFY" --project "$pass_project" >/dev/null

if "$VERIFY" --project "$missing_project" >/dev/null 2>&1; then
  printf '%s\n' 'FAIL: missing required category unexpectedly passed'
  exit 1
fi

HARNESS_REQUIRED_CHECKS=test "$VERIFY" --project "$override_project" >/dev/null

printf '%s\n' 'PASS: required-check enforcement'
