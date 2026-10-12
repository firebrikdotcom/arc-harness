#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-herdr-timeout.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

BIN_DIR="$TMP_ROOT/bin"
mkdir -p "$BIN_DIR"

cat > "$BIN_DIR/herdr" <<'EOF'
#!/bin/sh
cmd="$1"
sub="${2:-}"
case "$cmd" in
  pane)
    if [ "$sub" = "current" ]; then
      printf '{"result":{"pane":{"pane_id":"p-caller","tab_id":"t-caller","workspace_id":"ws-1"}}}\n'
    elif [ "$sub" = "get" ]; then
      printf '{"result":{"pane":{"agent_status":"working"}}}\n'
    fi
    ;;
  tab)
    if [ "$sub" = "get" ]; then
      printf '{"result":{"tab":{"tab_id":"t-caller","label":"my-feature"}}}\n'
    elif [ "$sub" = "list" ]; then
      printf '{"result":{"tabs":[{"label":"my-feature"}]}}\n'
    elif [ "$sub" = "create" ]; then
      printf '{"result":{"tab":{"tab_id":"t-rev-99"},"root_pane":{"pane_id":"p-rev-99"}}}\n'
    elif [ "$sub" = "close" ]; then
      printf '%s\n' "$3" > "$STUB_DIR/tab_closed_unexpectedly.txt"
      printf '{"result":{"closed":true}}\n'
    fi
    ;;
  agent)
    if [ "$sub" = "start" ]; then
      printf '{"result":{"status":"ok"}}\n'
    elif [ "$sub" = "prompt" ]; then
      # Simulates long-running prompt that does NOT produce findings before timeout
      printf '{"result":{"status":"working"}}\n'
    elif [ "$sub" = "get" ]; then
      printf '{"result":{"agent":{"status":"working"}}}\n'
    elif [ "$sub" = "read" ]; then
      printf 'still thinking...\n'
    fi
    ;;
esac
EOF
chmod +x "$BIN_DIR/herdr"

cat > "$BIN_DIR/claude" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$BIN_DIR/claude"

PATH="$BIN_DIR:$PATH"
export PATH
HERDR_ENV=1
export HERDR_ENV
STUB_DIR="$TMP_ROOT"
export STUB_DIR
HARNESS_REVIEWER_KIND=claude
export HARNESS_REVIEWER_KIND
# Set a tiny timeout so test runs fast
HARNESS_REVIEWER_TIMEOUT=1
export HARNESS_REVIEWER_TIMEOUT

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: hash-timeout\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"
OUT="$TMP_ROOT/out.txt"

status=0
"$REVIEWER" "$PACKET" "$FINDINGS" > "$OUT" 2>&1 || status=$?

[ "$status" -ne 0 ] || fail "reviewer should exit non-zero on timeout"
[ ! -f "$STUB_DIR/tab_closed_unexpectedly.txt" ] || fail "tab should NOT be closed on timeout"

# Must print tab label and pane ID so the user can look
grep -q "my-feature-reviewer" "$OUT" || fail "output missing tab label: $(cat "$OUT")"
grep -q "p-rev-99" "$OUT" || fail "output missing pane ID: $(cat "$OUT")"

printf 'PASS: Herdr timeout leaves tab open, prints label and pane ID, and exits non-zero\n'
