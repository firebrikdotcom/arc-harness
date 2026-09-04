#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
VERIFY="$HARNESS_ROOT_UNDER_TEST/scripts/verify.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-norewrite.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
OUT="$TMP_ROOT/out.txt"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

if ! command -v make >/dev/null 2>&1; then
  printf '%s\n' 'SKIP: make is unavailable'
  exit 0
fi

# A Makefile whose format target rewrites a file must never be run by verify.
PROJECT="$TMP_ROOT/make-project"
mkdir -p "$PROJECT"
printf 'format:\n\t@touch REWRITTEN\nfmt:\n\t@touch REWRITTEN\nformat-check:\n\t@true\n' > "$PROJECT/Makefile"
"$VERIFY" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed on make project"
[ ! -f "$PROJECT/REWRITTEN" ] || fail "verify ran a rewriting make format target"
grep -q 'make:format-check' "$OUT" || fail "verify should run the format-check target"

PROJECT2="$TMP_ROOT/make-only-format"
mkdir -p "$PROJECT2"
printf 'format:\n\t@touch REWRITTEN\n' > "$PROJECT2/Makefile"
"$VERIFY" --project "$PROJECT2" > "$OUT" 2>&1 || fail "verify failed on format-only make project"
[ ! -f "$PROJECT2/REWRITTEN" ] || fail "verify ran make format"
grep -q 'never runs a rewriting formatter' "$OUT" || fail "verify should explain the skipped format target"

# Same rule for a package.json with only a rewriting format script.
if command -v npm >/dev/null 2>&1 || command -v pnpm >/dev/null 2>&1 || command -v yarn >/dev/null 2>&1; then
  PROJECT3="$TMP_ROOT/node-project"
  mkdir -p "$PROJECT3"
  printf '%s\n' '{"name":"fixture","version":"1.0.0","scripts":{"format":"touch REWRITTEN"}}' > "$PROJECT3/package.json"
  "$VERIFY" --project "$PROJECT3" > "$OUT" 2>&1 || fail "verify failed on node project"
  [ ! -f "$PROJECT3/REWRITTEN" ] || fail "verify ran the rewriting npm format script"
  grep -q 'never runs a rewriting formatter' "$OUT" || fail "verify should explain the skipped format script"
fi

printf '%s\n' 'PASS: verify never runs a rewriting formatter'
