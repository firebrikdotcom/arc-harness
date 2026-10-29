#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INSTALL="$HARNESS_ROOT_UNDER_TEST/scripts/install-efficiency-rules.sh"
RULES="$HARNESS_ROOT_UNDER_TEST/docs/agent-efficiency.md"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-efficiency.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TMP_ROOT/home"
mkdir -p "$HOME_DIR/.claude" "$HOME_DIR/.codex"
CLAUDE_GUIDE="$HOME_DIR/.claude/CLAUDE.md"
CODEX_GUIDE="$HOME_DIR/.codex/AGENTS.md"

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

count() {
  grep -c -F "$1" "$2" || true
}

printf '%s\n' '# Before' '<!-- global-harness:start -->' 'Use the phases.' '<!-- global-harness:end -->' '# Other rules' 'Keep me.' > "$CLAUDE_GUIDE"
printf '%s\n' '# Codex guide without a harness block' 'Keep me too.' > "$CODEX_GUIDE"
CODEX_CONFIG="$HOME_DIR/.codex/config.toml"
printf '%s\n' 'model = "m"' '' '[features]' 'hooks = true' > "$CODEX_CONFIG"
cp "$CLAUDE_GUIDE" "$TMP_ROOT/claude.orig"
cp "$CODEX_GUIDE" "$TMP_ROOT/codex.orig"
cp "$CODEX_CONFIG" "$TMP_ROOT/config.orig"
# A stub stands in for Codex's own check of the settings.
printf '%s\n' '#!/bin/sh' 'exit 0' > "$TMP_ROOT/codex-ok"
printf '%s\n' '#!/bin/sh' 'exit 1' > "$TMP_ROOT/codex-rejects"
chmod +x "$TMP_ROOT/codex-ok" "$TMP_ROOT/codex-rejects"
HARNESS_CODEX_BIN="$TMP_ROOT/codex-ok"
export HARNESS_CODEX_BIN

# First run: one block in each guide, after the global block or at the end, with the rules text.
"$INSTALL" --home "$HOME_DIR" >/dev/null
for f in "$CLAUDE_GUIDE" "$CODEX_GUIDE"; do
  [ "$(count '<!-- harness-efficiency:start -->' "$f")" -eq 1 ] || fail "$f should hold exactly one block"
  grep -qF "$(head -1 "$RULES")" "$f" || fail "rules heading missing in $f"
  grep -q 'git worktree remove' "$f" || fail "worktree cleanup rule missing in $f"
done
awk '/global-harness:end/{seen=1} /harness-efficiency:start/{ if (!seen) exit 1 }' "$CLAUDE_GUIDE" ||
  fail "block should follow the global block"
grep -q 'Keep me.' "$CLAUDE_GUIDE" || fail "content after the global block was lost"
tail -1 "$CODEX_GUIDE" | grep -qF '<!-- harness-efficiency:end -->' || fail "block should be appended to a guide without a global block"

[ "$(count '[features.multi_agent_v2]' "$CODEX_CONFIG")" -eq 1 ] || fail "Codex config should hold the helper settings once"
grep -q '^min_wait_timeout_ms = 1800000$' "$CODEX_CONFIG" || fail "long wait missing from the Codex config"
grep -q 'subagent_developer_instructions' "$CODEX_CONFIG" || fail "helper instructions missing from the Codex config"
grep -q '^hooks = true$' "$CODEX_CONFIG" || fail "existing Codex settings were lost"

# Second run changes nothing.
for f in "$CLAUDE_GUIDE" "$CODEX_GUIDE" "$CODEX_CONFIG"; do cp "$f" "$f.before"; done
"$INSTALL" --home "$HOME_DIR" >/dev/null
for f in "$CLAUDE_GUIDE" "$CODEX_GUIDE" "$CODEX_CONFIG"; do
  cmp -s "$f" "$f.before" || fail "second run modified $f"
done

# A stale block is replaced by the current rules.
awk '{print} /harness-efficiency:start/{print "stale rule"}' "$CLAUDE_GUIDE.before" > "$CLAUDE_GUIDE"
"$INSTALL" --home "$HOME_DIR" >/dev/null
grep -q 'stale rule' "$CLAUDE_GUIDE" && fail "stale block should be replaced"
cmp -s "$CLAUDE_GUIDE" "$CLAUDE_GUIDE.before" || fail "refresh should match a fresh install"

# --remove restores both guides exactly.
"$INSTALL" --home "$HOME_DIR" --remove >/dev/null
cmp -s "$CLAUDE_GUIDE" "$TMP_ROOT/claude.orig" || fail "--remove should restore the Claude guide"
cmp -s "$CODEX_GUIDE" "$TMP_ROOT/codex.orig" || fail "--remove should restore the Codex guide"
cmp -s "$CODEX_CONFIG" "$TMP_ROOT/config.orig" || fail "--remove should restore the Codex config"

# Codex rejecting the settings restores the config and fails the install.
if HARNESS_CODEX_BIN="$TMP_ROOT/codex-rejects" "$INSTALL" --home "$HOME_DIR" >/dev/null; then
  fail "a rejected Codex config should fail the install"
fi
cmp -s "$CODEX_CONFIG" "$TMP_ROOT/config.orig" || fail "a rejected Codex config should be restored"
"$INSTALL" --home "$HOME_DIR" --remove >/dev/null

# A config that sets the table itself is left alone.
printf '%s\n' '[features.multi_agent_v2]' 'min_wait_timeout_ms = 1000' >> "$CODEX_CONFIG"
cp "$CODEX_CONFIG" "$TMP_ROOT/config.own"
"$INSTALL" --home "$HOME_DIR" | grep -q 'sets \[features.multi_agent_v2\] itself' || fail "own table should be reported"
cmp -s "$CODEX_CONFIG" "$TMP_ROOT/config.own" || fail "own table should be left alone"
"$INSTALL" --home "$HOME_DIR" --remove >/dev/null
rm -f "$CODEX_CONFIG"

# A guide that links to a shared copy keeps its link; the shared copy gets the block.
mkdir -p "$TMP_ROOT/vault"
cp "$TMP_ROOT/codex.orig" "$TMP_ROOT/vault/AGENTS.md"
ln -sfn "$TMP_ROOT/vault/AGENTS.md" "$CODEX_GUIDE"
"$INSTALL" --home "$HOME_DIR" >/dev/null
[ -L "$CODEX_GUIDE" ] || fail "a linked guide should stay a link"
grep -q 'harness-efficiency:start' "$TMP_ROOT/vault/AGENTS.md" || fail "the linked copy should hold the block"
rm -f "$CODEX_GUIDE"

# A missing guide is skipped, and a missing home is a usage error.
rm -f "$CODEX_GUIDE"
"$INSTALL" --home "$HOME_DIR" >/dev/null
[ -f "$CODEX_GUIDE" ] && fail "a missing guide must not be created"
if "$INSTALL" --home "$TMP_ROOT/nowhere" >/dev/null 2>&1; then
  fail "a missing home should fail"
fi

printf '%s\n' 'install-efficiency-rules: ok'
