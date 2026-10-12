#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-headless.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

BIN_DIR="$TMP_ROOT/bin"
mkdir -p "$BIN_DIR"

cat > "$BIN_DIR/claude" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$STUB_LOG_DIR/claude.args"
printf '```json\n{"tree_hash":"tree1","reviewer":"claude-headless","verdict":"approve","findings":[]}\n```\n'
EOF
chmod +x "$BIN_DIR/claude"

cat > "$BIN_DIR/codex" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$STUB_LOG_DIR/codex.args"
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ] || [ "$1" = "--output-last-message" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
if [ -n "$out" ]; then
  printf '{"tree_hash":"tree2","reviewer":"codex-headless","verdict":"approve","findings":[]}\n' > "$out"
fi
EOF
chmod +x "$BIN_DIR/codex"

PATH="$BIN_DIR:$PATH"
export PATH
HERDR_ENV=0
export HERDR_ENV
STUB_LOG_DIR="$TMP_ROOT"
export STUB_LOG_DIR

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: tree1\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"

# 1. Claude headless: uses -p, read-only tools, writes pure JSON into FINDINGS
rm -f "$FINDINGS" "$TMP_ROOT/claude.args"
HARNESS_REVIEWER_KIND=claude "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$FINDINGS" ] || fail "claude headless did not produce FINDINGS_OUT"
grep -q -E '"tree_hash":[[:space:]]*"tree1"' "$FINDINGS" || fail "findings should contain pure JSON parsed from claude output"
grep -q -- "-p" "$TMP_ROOT/claude.args" || grep -q -- "--print" "$TMP_ROOT/claude.args" || fail "claude should be called with -p"
grep -q -- "--tools" "$TMP_ROOT/claude.args" || fail "claude should specify read-only tools"

# Prompt check: contains packet path and instructions, no other conversation context
grep -q "$PACKET" "$TMP_ROOT/claude.args" || fail "prompt must hold the packet path"
grep -q "Read the packet and follow it\. Do not edit any project file\. Write only the findings JSON to" "$TMP_ROOT/claude.args" || fail "prompt must hold exact instructions"

# 2. Codex headless: uses exec --sandbox read-only -o FINDINGS_OUT
rm -f "$FINDINGS" "$TMP_ROOT/codex.args"
HARNESS_REVIEWER_KIND=codex "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$FINDINGS" ] || fail "codex headless did not produce FINDINGS_OUT"
grep -q -E '"tree_hash":[[:space:]]*"tree2"' "$FINDINGS" || fail "findings should contain pure JSON from codex"
grep -q -- "exec" "$TMP_ROOT/codex.args" || fail "codex should run exec subcommand"
grep -q -- "read-only" "$TMP_ROOT/codex.args" || fail "codex should use read-only sandbox"
grep -q -- "$FINDINGS" "$TMP_ROOT/codex.args" || fail "codex should receive -o FINDINGS_OUT"
grep -q "$PACKET" "$TMP_ROOT/codex.args" || fail "codex prompt must hold packet path"

printf 'PASS: reviewer headless execution for claude and codex\n'
