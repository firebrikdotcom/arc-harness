#!/usr/bin/env sh
# Verification must produce evidence: a run where no check ran fails unless the
# project declares allow-empty, a registered target is held to the categories
# init detected, and a verify record stops counting once project files change.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-evidence.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_REQUIRED_CHECKS HARNESS_ROOT || true
OUT="$TMP_ROOT/out.txt"
VERIFY="$ROOT/scripts/verify.sh"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

# An empty project: nothing ran, so nothing was proven.
EMPTY="$TMP_ROOT/empty"
mkdir -p "$EMPTY"
status=0
"$VERIFY" --project "$EMPTY" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "verify with no checks should fail"
grep -q 'no checks ran' "$OUT" || fail "verify should say no checks ran"
grep -q '^EXIT=1$' "$HARNESS_DB_ROOT/records/verify.state" || fail "the record should show the failure"

# allow-empty is an explicit, visible exemption.
printf '%s\n' 'allow-empty  # documentation-only project' > "$EMPTY/.harness-required-checks"
"$VERIFY" --project "$EMPTY" > "$OUT" 2>&1 || fail "allow-empty should accept a run with no checks"
grep -q "accepted because 'allow-empty' is declared" "$OUT" || fail "the exemption should be reported"

# init records the categories a target has tooling for, in the target database only.
TARGET="$TMP_ROOT/target"
mkdir -p "$TARGET"
git -C "$TARGET" init -q
printf 'test:\n\t@true\n' > "$TARGET/Makefile"
"$ROOT/scripts/init.sh" --project "$TARGET" --yes > "$OUT" 2>&1 || fail "init failed"
TDIR=$("$ROOT/scripts/harness-target.sh" lookup "$TARGET")
grep -qx 'test' "$TDIR/db/required-checks" || fail "init should record the test category"
[ ! -e "$TARGET/.harness-required-checks" ] || fail "init must not write into the project"
"$VERIFY" --project "$TARGET" > "$OUT" 2>&1 || fail "verify should pass with the detected test target"
grep -q "Required checks ($TDIR/db/required-checks): test" "$OUT" || fail "verify should use the recorded categories"
printf 'lint:\n\t@true\n' > "$TARGET/Makefile"
status=0
"$VERIFY" --project "$TARGET" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "removing the recorded test category's target should fail verification"
grep -q "required category 'test' ran no checks" "$OUT" || fail "verify should name the missing category"

# A verify record covers the files as they were: an edit afterwards voids it.
printf 'test:\n\t@true\n' > "$TARGET/Makefile"
git -C "$TARGET" add Makefile
git -C "$TARGET" -c user.email=t@example.com -c user.name=t commit -q -m init
CLI="$ROOT/scripts/harness"
(cd "$TARGET" && "$CLI" plan start && "$CLI" plan "done" && "$CLI" build start) > "$OUT" 2>&1 || fail "phases failed"
"$VERIFY" --project "$TARGET" > "$OUT" 2>&1 || fail "verify failed"
grep -q '^TREE_HASH=[0-9a-f]\{40\}' "$TDIR/db/records/verify.state" || fail "the record should carry a tree hash"
printf '%s\n' 'late edit' > "$TARGET/notes.txt"
status=0
(cd "$TARGET" && "$CLI" build "done") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 4 ] || fail "build done should refuse a verify that predates an edit, exited $status"
grep -q 'project files changed after the last verify run' "$OUT" || fail "the refusal should explain the stale record"
# Writes outside the project do not void it.
rm "$TARGET/notes.txt"
printf '%s\n' 'scratch' > "$TMP_ROOT/outside.txt"
(cd "$TARGET" && "$CLI" build "done") > "$OUT" 2>&1 || fail "an unchanged project should keep its verify record"

printf '%s\n' 'PASS: verification needs evidence, honours recorded categories, and expires on project edits'
