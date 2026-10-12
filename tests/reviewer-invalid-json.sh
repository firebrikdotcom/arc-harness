#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-invalid-json.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

BIN_DIR="$TMP_ROOT/bin"
mkdir -p "$BIN_DIR"

cat > "$BIN_DIR/claude" <<'EOF'
#!/bin/sh
printf 'This is plain English text, not JSON at all.\n'
EOF
chmod +x "$BIN_DIR/claude"

cat > "$BIN_DIR/codex" <<'EOF'
#!/bin/sh
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
  printf '{not valid json: true\n' > "$out"
fi
EOF
chmod +x "$BIN_DIR/codex"

cat > "$BIN_DIR/herdr" <<'EOF'
#!/bin/sh
cmd="$1"
sub="${2:-}"
case "$cmd" in
  pane)
    if [ "$sub" = "current" ]; then
      printf '{"result":{"pane":{"pane_id":"p-bad","tab_id":"t-bad","workspace_id":"ws-1"}}}\n'
    fi
    ;;
  tab)
    if [ "$sub" = "get" ]; then
      printf '{"result":{"tab":{"tab_id":"t-bad","label":"bad-json"}}}\n'
    elif [ "$sub" = "list" ]; then
      printf '{"result":{"tabs":[{"label":"bad-json"}]}}\n'
    elif [ "$sub" = "create" ]; then
      printf '{"result":{"tab":{"tab_id":"t-bad-rev"},"root_pane":{"pane_id":"p-bad-rev"}}}\n'
    elif [ "$sub" = "close" ]; then
      printf 'closed\n' > "$STUB_DIR/tab_closed_bad.txt"
    fi
    ;;
  agent)
    if [ "$sub" = "start" ]; then
      printf '{"result":{"status":"ok"}}\n'
    elif [ "$sub" = "prompt" ]; then
      printf 'malformed json content\n' > "$FINDINGS_DEST"
      printf '{"result":{"status":"done"}}\n'
    fi
    ;;
esac
EOF
chmod +x "$BIN_DIR/herdr"

PATH="$BIN_DIR:$PATH"
export PATH
STUB_DIR="$TMP_ROOT"
export STUB_DIR

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: test\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"
FINDINGS_DEST="$FINDINGS"
export FINDINGS_DEST

# 1. Headless Claude with non-JSON output -> exit non-zero
HERDR_ENV=0
export HERDR_ENV
status=0
HARNESS_REVIEWER_KIND=claude "$REVIEWER" "$PACKET" "$FINDINGS" >/dev/null 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "headless claude with invalid JSON should exit non-zero"

# 2. Headless Codex with non-JSON output -> exit non-zero
status=0
HARNESS_REVIEWER_KIND=codex "$REVIEWER" "$PACKET" "$FINDINGS" >/dev/null 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "headless codex with invalid JSON should exit non-zero"

# 3. Herdr mode with non-JSON findings -> exit non-zero and leave tab open
HERDR_ENV=1
export HERDR_ENV
HARNESS_REVIEWER_TIMEOUT=2
export HARNESS_REVIEWER_TIMEOUT
rm -f "$STUB_DIR/tab_closed_bad.txt"
status=0
OUT="$TMP_ROOT/herdr-bad.out"
HARNESS_REVIEWER_KIND=claude "$REVIEWER" "$PACKET" "$FINDINGS" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "herdr mode with invalid JSON should exit non-zero"
[ ! -f "$STUB_DIR/tab_closed_bad.txt" ] || fail "tab should NOT be closed when findings are invalid JSON"
grep -q "bad-json-reviewer" "$OUT" || fail "output missing tab label on invalid JSON: $(cat "$OUT")"
grep -q "p-bad-rev" "$OUT" || fail "output missing pane ID on invalid JSON: $(cat "$OUT")"

printf 'PASS: invalid JSON rejected in both headless and Herdr modes\n'
