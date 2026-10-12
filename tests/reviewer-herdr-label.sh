#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEWER="$HARNESS_ROOT/scripts/reviewer.sh"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-herdr-label.XXXXXX")
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
      printf '{"result":{"pane":{"pane_id":"p1","tab_id":"t1","workspace_id":"w1"}}}\n'
    fi
    ;;
  tab)
    if [ "$sub" = "get" ]; then
      printf '{"result":{"tab":{"tab_id":"t1","label":"feature-auth"}}}\n'
    elif [ "$sub" = "list" ]; then
      # Read existing labels from environment or state file
      cat "$STUB_DIR/tab_list.json"
    elif [ "$sub" = "create" ]; then
      label=""
      while [ "$#" -gt 0 ]; do
        if [ "$1" = "--label" ]; then
          label="$2"
          shift 2
        else
          shift
        fi
      done
      printf '%s\n' "$label" > "$STUB_DIR/created_label.txt"
      printf '{"result":{"tab":{"tab_id":"t99"},"root_pane":{"pane_id":"p99"}}}\n'
    elif [ "$sub" = "close" ]; then
      printf '{"result":{"closed":true}}\n'
    fi
    ;;
  agent)
    if [ "$sub" = "start" ]; then
      printf '{"result":{"status":"ok"}}\n'
    elif [ "$sub" = "prompt" ]; then
      # Immediately write valid findings to test target
      printf '{"tree_hash":"h1","reviewer":"stub","verdict":"approve","findings":[]}\n' > "$FINDINGS_DEST"
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

PATH="$BIN_DIR:$PATH"
export PATH
HERDR_ENV=1
export HERDR_ENV
STUB_DIR="$TMP_ROOT"
export STUB_DIR
HARNESS_REVIEWER_KIND=claude
export HARNESS_REVIEWER_KIND
HARNESS_REVIEWER_TIMEOUT=5
export HARNESS_REVIEWER_TIMEOUT

PACKET="$TMP_ROOT/packet.md"
printf 'Tree hash: h1\n' > "$PACKET"
FINDINGS="$TMP_ROOT/findings.json"
FINDINGS_DEST="$FINDINGS"
export FINDINGS_DEST

# Case 1: no existing reviewer tab -> feature-auth-reviewer
printf '{"result":{"tabs":[{"label":"feature-auth"}]}}\n' > "$STUB_DIR/tab_list.json"
rm -f "$STUB_DIR/created_label.txt" "$FINDINGS"
"$REVIEWER" "$PACKET" "$FINDINGS"
[ -f "$STUB_DIR/created_label.txt" ] || fail "tab create was not called"
created=$(cat "$STUB_DIR/created_label.txt")
[ "$created" = "feature-auth-reviewer" ] || fail "expected label 'feature-auth-reviewer', got '$created'"

# Case 2: feature-auth-reviewer exists -> feature-auth-reviewer-2
printf '{"result":{"tabs":[{"label":"feature-auth"},{"label":"feature-auth-reviewer"}]}}\n' > "$STUB_DIR/tab_list.json"
rm -f "$STUB_DIR/created_label.txt" "$FINDINGS"
"$REVIEWER" "$PACKET" "$FINDINGS"
created=$(cat "$STUB_DIR/created_label.txt")
[ "$created" = "feature-auth-reviewer-2" ] || fail "expected label 'feature-auth-reviewer-2', got '$created'"

# Case 3: feature-auth-reviewer-2 also exists -> feature-auth-reviewer-3
printf '{"result":{"tabs":[{"label":"feature-auth"},{"label":"feature-auth-reviewer"},{"label":"feature-auth-reviewer-2"}]}}\n' > "$STUB_DIR/tab_list.json"
rm -f "$STUB_DIR/created_label.txt" "$FINDINGS"
"$REVIEWER" "$PACKET" "$FINDINGS"
created=$(cat "$STUB_DIR/created_label.txt")
[ "$created" = "feature-auth-reviewer-3" ] || fail "expected label 'feature-auth-reviewer-3', got '$created'"

printf 'PASS: Herdr tab label <label>-reviewer and collision suffixes\n'
