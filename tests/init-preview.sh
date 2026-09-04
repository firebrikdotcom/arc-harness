#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INIT="$HARNESS_ROOT_UNDER_TEST/scripts/init.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-init.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
OUT="$TMP_ROOT/out.txt"
PROJECT="$TMP_ROOT/project"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

if ! command -v make >/dev/null 2>&1; then
  printf '%s\n' 'SKIP: make is unavailable; init preview test needs a Makefile target'
  exit 0
fi

# A project whose own setup target leaves a marker file when it runs.
mkdir -p "$PROJECT"
printf 'init:\n\t@touch RAN_PROJECT_SETUP\n' > "$PROJECT/Makefile"

# Without a terminal and without --yes: preview, refuse, run nothing.
status=0
"$INIT" --project "$PROJECT" > "$OUT" 2>&1 < /dev/null || status=$?
[ "$status" -eq 3 ] || fail "init without confirmation exited $status, expected 3"
expect_output 'make init'
expect_output 'REFUSED'
[ ! -f "$PROJECT/RAN_PROJECT_SETUP" ] || fail "project setup ran without confirmation"

# Explicit "no" at the prompt also runs nothing.
status=0
printf 'n\n' | "$INIT" --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 3 ] || fail "init after answering no exited $status, expected 3"
[ ! -f "$PROJECT/RAN_PROJECT_SETUP" ] || fail "project setup ran after answering no"

# --yes runs the previewed commands.
"$INIT" --project "$PROJECT" --yes > "$OUT" 2>&1 || fail "init --yes failed"
[ -f "$PROJECT/RAN_PROJECT_SETUP" ] || fail "project setup did not run with --yes"

# HARNESS_INIT_YES=1 is the CI form.
rm -f "$PROJECT/RAN_PROJECT_SETUP"
HARNESS_INIT_YES=1 "$INIT" --project "$PROJECT" > "$OUT" 2>&1 < /dev/null || fail "init with HARNESS_INIT_YES failed"
[ -f "$PROJECT/RAN_PROJECT_SETUP" ] || fail "project setup did not run with HARNESS_INIT_YES=1"

# A project with nothing to run needs no confirmation.
mkdir -p "$TMP_ROOT/empty"
"$INIT" --project "$TMP_ROOT/empty" > "$OUT" 2>&1 < /dev/null || fail "init on an empty project should pass"
expect_output 'nothing to run'

printf '%s\n' 'PASS: init previews project-owned commands and runs them only after confirmation'
