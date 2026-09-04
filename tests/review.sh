#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEW="$HARNESS_ROOT_UNDER_TEST/scripts/review.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-review.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
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

# A git project with a staged change, an unstaged change, and an untracked file.
mkdir -p "$PROJECT/scripts"
git -C "$PROJECT" init -q
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q --allow-empty -m init
printf '%s\n' 'tracked original' > "$PROJECT/tracked.txt"
printf '%s\n' 'staged original' > "$PROJECT/staged.txt"
git -C "$PROJECT" add tracked.txt staged.txt
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m files
printf '%s\n' 'UNSTAGED_EDIT_LINE' >> "$PROJECT/tracked.txt"
printf '%s\n' 'STAGED_EDIT_LINE' >> "$PROJECT/staged.txt"
git -C "$PROJECT" add staged.txt
printf '%s\n' 'UNTRACKED_NEW_LINE' > "$PROJECT/new.txt"

# Passing verify: review shows every kind of change and records exit 0.
status=0
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "review exited $status with a passing verify"
expect_output '^+UNSTAGED_EDIT_LINE'
expect_output '^+STAGED_EDIT_LINE'
expect_output '^+UNTRACKED_NEW_LINE'
expect_output 'Review finished\.'
grep -q '^EXIT=0$' "$HARNESS_DB_ROOT/records/review.state" || fail "review record should show EXIT=0"

# Failing verify: review keeps going, still shows the patch, exits non-zero, records the failure.
printf '%s\n' 'lint' > "$PROJECT/.harness-required-checks"
status=0
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "review should exit non-zero when verify fails"
expect_output 'WARN: verification failed'
expect_output '^+UNSTAGED_EDIT_LINE'
expect_output '^+UNTRACKED_NEW_LINE'
expect_output 'Review questions:'
grep -q '^EXIT=1$' "$HARNESS_DB_ROOT/records/review.state" || fail "review record should show EXIT=1"

printf '%s\n' 'PASS: review shows the full patch and continues after a failing verify'
