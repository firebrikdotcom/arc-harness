#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-detection.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

# Create stub claude and codex binaries
BIN_DIR="$TMP_ROOT/bin"
mkdir -p "$BIN_DIR"

cat > "$BIN_DIR/claude" <<'EOF'
#!/bin/sh
echo "$*" > "$STUB_LOG_DIR/claude.called"
printf '{"tree_hash":"123","reviewer":"claude","verdict":"approve","findings":[]}\n'
EOF
chmod +x "$BIN_DIR/claude"

cat > "$BIN_DIR/codex" <<'EOF'
#!/bin/sh
echo "$*" > "$STUB_LOG_DIR/codex.called"
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
  printf '{"tree_hash":"123","reviewer":"codex","verdict":"approve","findings":[]}\n' > "$out"
fi
EOF
chmod +x "$BIN_DIR/codex"

PATH="$BIN_DIR:$PATH"
export PATH
HERDR_ENV=0
export HERDR_ENV

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: 123\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"
STUB_LOG_DIR="$TMP_ROOT"
export STUB_LOG_DIR

# 1. HARNESS_REVIEWER_KIND=claude selects claude
rm -f "$TMP_ROOT/claude.called" "$TMP_ROOT/codex.called" "$FINDINGS"
HARNESS_REVIEWER_KIND=claude CLAUDE_SESSION_ID="" CODEX_THREAD_ID="" "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$TMP_ROOT/claude.called" ] || fail "claude should have been called when HARNESS_REVIEWER_KIND=claude"
[ ! -f "$TMP_ROOT/codex.called" ] || fail "codex should not have been called when HARNESS_REVIEWER_KIND=claude"

# 2. HARNESS_REVIEWER_KIND=codex selects codex
rm -f "$TMP_ROOT/claude.called" "$TMP_ROOT/codex.called" "$FINDINGS"
HARNESS_REVIEWER_KIND=codex CLAUDE_SESSION_ID="" CODEX_THREAD_ID="" "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$TMP_ROOT/codex.called" ] || fail "codex should have been called when HARNESS_REVIEWER_KIND=codex"
[ ! -f "$TMP_ROOT/claude.called" ] || fail "claude should not have been called when HARNESS_REVIEWER_KIND=codex"

# 3. Environment detection: CLAUDE_SESSION_ID selects claude
rm -f "$TMP_ROOT/claude.called" "$TMP_ROOT/codex.called" "$FINDINGS"
HARNESS_REVIEWER_KIND="" CLAUDE_SESSION_ID="sess-1" CODEX_THREAD_ID="" "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$TMP_ROOT/claude.called" ] || fail "CLAUDE_SESSION_ID should detect claude"

# 4. Environment detection: CODEX_THREAD_ID selects codex
rm -f "$TMP_ROOT/claude.called" "$TMP_ROOT/codex.called" "$FINDINGS"
HARNESS_REVIEWER_KIND="" CLAUDE_SESSION_ID="" CODEX_THREAD_ID="th-1" "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$TMP_ROOT/codex.called" ] || fail "CODEX_THREAD_ID should detect codex"

# 5. Model and effort flags pass through
rm -f "$TMP_ROOT/claude.called" "$TMP_ROOT/codex.called" "$FINDINGS"
HARNESS_REVIEWER_KIND=claude HARNESS_REVIEWER_MODEL="test-model" HARNESS_REVIEWER_EFFORT="high" "$REVIEWER" "$PACKET" "$FINDINGS"
grep -q -- "--model test-model" "$TMP_ROOT/claude.called" || grep -q -- "-m test-model" "$TMP_ROOT/claude.called" || fail "model flag not passed to claude"
grep -q -- "--effort high" "$TMP_ROOT/claude.called" || fail "effort flag not passed to claude"

rm -f "$TMP_ROOT/claude.called" "$TMP_ROOT/codex.called" "$FINDINGS"
HARNESS_REVIEWER_KIND=codex HARNESS_REVIEWER_MODEL="test-codex-model" HARNESS_REVIEWER_EFFORT="medium" "$REVIEWER" "$PACKET" "$FINDINGS"
grep -q -- "test-codex-model" "$TMP_ROOT/codex.called" || fail "model flag not passed to codex"
grep -q -- "medium" "$TMP_ROOT/codex.called" || fail "effort flag not passed to codex"

printf 'PASS: reviewer runtime detection and option passthrough\n'
