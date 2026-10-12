#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-herdr-success.XXXXXX")
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
      printf '{"result":{"pane":{"pane_id":"caller-pane-1","tab_id":"caller-tab-1","workspace_id":"ws-1"}}}\n'
    fi
    ;;
  tab)
    if [ "$sub" = "get" ]; then
      printf '{"result":{"tab":{"tab_id":"caller-tab-1","label":"main-work"}}}\n'
    elif [ "$sub" = "list" ]; then
      printf '{"result":{"tabs":[{"label":"main-work"}]}}\n'
    elif [ "$sub" = "create" ]; then
      printf '%s\n' "$@" > "$STUB_DIR/tab_create.args"
      printf '{"result":{"tab":{"tab_id":"new-rev-tab-42"},"root_pane":{"pane_id":"new-rev-pane-42"}}}\n'
    elif [ "$sub" = "close" ]; then
      printf '%s\n' "$3" > "$STUB_DIR/tab_closed.txt"
      printf '{"result":{"closed":true}}\n'
    fi
    ;;
  agent)
    if [ "$sub" = "start" ]; then
      echo "$*" > "$STUB_DIR/agent_start.args"
      printf '{"result":{"status":"ok"}}\n'
    elif [ "$sub" = "prompt" ]; then
      printf '%s\n' "$@" > "$STUB_DIR/agent_prompt.args"
      # Simulate reviewer producing valid JSON in FINDINGS_DEST
      printf '{"tree_hash":"hash-success","reviewer":"herdr-agent","verdict":"approve","findings":[]}\n' > "$FINDINGS_DEST"
      printf '{"result":{"status":"done"}}\n'
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

cat > "$BIN_DIR/codex" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$BIN_DIR/codex"

PATH="$BIN_DIR:$PATH"
export PATH
HERDR_ENV=1
export HERDR_ENV
STUB_DIR="$TMP_ROOT"
export STUB_DIR
HARNESS_REVIEWER_TIMEOUT=5
export HARNESS_REVIEWER_TIMEOUT

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: hash-success\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"
FINDINGS_DEST="$FINDINGS"
export FINDINGS_DEST

# 1. Claude in Herdr: starts agent with --permission-mode auto, prompts, closes created tab on success
rm -f "$STUB_DIR/tab_closed.txt" "$FINDINGS"
HARNESS_REVIEWER_KIND=claude "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$FINDINGS" ] || fail "findings was not produced"
[ -f "$STUB_DIR/tab_closed.txt" ] || fail "new tab was not closed on success"
closed=$(cat "$STUB_DIR/tab_closed.txt")
[ "$closed" = "new-rev-tab-42" ] || fail "expected tab new-rev-tab-42 to be closed, got '$closed'"
grep -q -- "--permission-mode auto" "$STUB_DIR/agent_start.args" || fail "Claude should be started with --permission-mode auto"

# 2. Codex in Herdr: starts agent with --no-daemon --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust
rm -f "$STUB_DIR/tab_closed.txt" "$FINDINGS"
HARNESS_REVIEWER_KIND=codex "$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$FINDINGS" ] || fail "findings was not produced"
[ -f "$STUB_DIR/tab_closed.txt" ] || fail "new tab was not closed on success for codex"
grep -q -- "--no-daemon" "$STUB_DIR/agent_start.args" || fail "Codex missing --no-daemon"
grep -q -- "--dangerously-bypass-approvals-and-sandbox" "$STUB_DIR/agent_start.args" || fail "Codex missing bypass flag"
grep -q -- "--dangerously-bypass-hook-trust" "$STUB_DIR/agent_start.args" || fail "Codex missing bypass hook trust"

printf 'PASS: Herdr mode closes reviewer tab on success with correct agent flags\n'
