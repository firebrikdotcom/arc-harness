#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-unknown.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: 123\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"
OUT="$TMP_ROOT/out.txt"

# 1. Neither HARNESS_REVIEWER_KIND nor environment variables set -> exit non-zero
status=0
HARNESS_REVIEWER_KIND="" CLAUDE_SESSION_ID="" CLAUDE_CODE_SESSION_ID="" CLAUDECODE="" CLAUDE_ENV_FILE="" CODEX_THREAD_ID="" \
  "$REVIEWER" "$PACKET" "$FINDINGS" > "$OUT" 2>&1 || status=$?

[ "$status" -ne 0 ] || fail "reviewer should exit non-zero when runtime cannot be detected"
grep -q -i "runtime" "$OUT" || fail "output should explain that runtime could not be detected"

# 2. Invalid HARNESS_REVIEWER_KIND value -> exit non-zero
status=0
HARNESS_REVIEWER_KIND="invalid_runtime" CLAUDE_SESSION_ID="" CODEX_THREAD_ID="" \
  "$REVIEWER" "$PACKET" "$FINDINGS" > "$OUT" 2>&1 || status=$?

[ "$status" -ne 0 ] || fail "reviewer should exit non-zero when HARNESS_REVIEWER_KIND is invalid"

printf 'PASS: reviewer refuses when runtime is unknown\n'
