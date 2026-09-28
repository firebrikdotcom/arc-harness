#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INSTALL="$HARNESS_ROOT_UNDER_TEST/scripts/install-jg-skill.sh"
WRAPPER="$HARNESS_ROOT_UNDER_TEST/scripts/jg.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-jg-skill.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TMP_ROOT/home"
UPSTREAM="$TMP_ROOT/upstream/SKILL.md"
mkdir -p "$HOME_DIR/.claude" "$HOME_DIR/.codex" "$TMP_ROOT/upstream"
CLAUDE_SKILL="$HOME_DIR/.claude/skills/jevgrep/SKILL.md"
CODEX_SKILL="$HOME_DIR/.agents/skills/jevgrep/SKILL.md"
CLAUDE_GUIDE="$HOME_DIR/.claude/CLAUDE.md"
CODEX_GUIDE="$HOME_DIR/.codex/AGENTS.md"

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

count() {
  grep -c -F "$1" "$2" || true
}

printf '%s\n' '---' 'name: jevgrep' 'description: upstream skill' '---' '' '# Jevgrep' '' 'jg "question" .' > "$UPSTREAM"
printf '%s\n' '<!-- global-harness:start -->' '## Global development harness' 'Use the phases.' '<!-- global-harness:end -->' '' '# Other rules' 'Keep me.' > "$CLAUDE_GUIDE"
printf '%s\n' '# Unrelated codex guide without a harness block' > "$CODEX_GUIDE"

# First run: both skills created from upstream plus the harness block; the
# Claude global guide gains the paragraph inside its block, the Codex guide
# without a block is left alone.
"$INSTALL" --home "$HOME_DIR" --source "$UPSTREAM" >/dev/null
for f in "$CLAUDE_SKILL" "$CODEX_SKILL"; do
  [ -f "$f" ] || fail "skill not created: $f"
  grep -q 'description: upstream skill' "$f" || fail "upstream content missing in $f"
  grep -q 'jg "question" .' "$f" || fail "upstream search example missing in $f"
  [ "$(count '<!-- harness-jg:start -->' "$f")" -eq 1 ] || fail "$f should hold exactly one harness block"
  grep -qF "$WRAPPER --project /absolute/path/to/project \"question\"" "$f" || fail "wrapper command missing in $f"
  grep -q 'do not run .jg. directly' "$f" || fail "redirect sentence missing in $f"
  grep -q 'harness-no-upload' "$f" || fail "opt-out marker missing in $f"
done
[ "$(count '<!-- harness-jg:start -->' "$CLAUDE_GUIDE")" -eq 1 ] || fail "global Claude guide should hold one retrieval paragraph"
grep -qF "$WRAPPER --project" "$CLAUDE_GUIDE" || fail "global Claude guide missing the wrapper command"
grep -q 'Keep me.' "$CLAUDE_GUIDE" || fail "content after the global block was lost"
awk '/harness-jg:start/{seen=1} /global-harness:end/{ if (!seen) exit 1 }' "$CLAUDE_GUIDE" || fail "paragraph should sit before the global end marker"
grep -q 'harness-jg' "$CODEX_GUIDE" && fail "codex guide without a harness block must not be edited"

# Second run changes nothing.
for f in "$CLAUDE_SKILL" "$CODEX_SKILL" "$CLAUDE_GUIDE"; do cp "$f" "$f.before"; done
"$INSTALL" --home "$HOME_DIR" --source "$UPSTREAM" >/dev/null
for f in "$CLAUDE_SKILL" "$CODEX_SKILL" "$CLAUDE_GUIDE"; do
  cmp -s "$f" "$f.before" || fail "second run modified $f"
done

# A stale block is replaced, in the skill and in the global guide.
printf '%s\n' '---' 'name: jevgrep' '---' 'upstream part' '' '<!-- harness-jg:start -->' 'old block' '<!-- harness-jg:end -->' > "$CODEX_SKILL"
printf '%s\n' '<!-- global-harness:start -->' 'Phases.' '<!-- harness-jg:start -->' 'old paragraph' '<!-- harness-jg:end -->' '<!-- global-harness:end -->' > "$CLAUDE_GUIDE"
"$INSTALL" --home "$HOME_DIR" --skip-global --source "$UPSTREAM" >/dev/null
grep -q 'old block' "$CODEX_SKILL" && fail "stale skill block should be replaced"
grep -q 'old paragraph' "$CLAUDE_GUIDE" || fail "--skip-global must leave the global guide alone"
"$INSTALL" --home "$HOME_DIR" --source "$UPSTREAM" >/dev/null
grep -q 'old paragraph' "$CLAUDE_GUIDE" && fail "stale global paragraph should be replaced"
[ "$(count '<!-- harness-jg:start -->' "$CLAUDE_GUIDE")" -eq 1 ] || fail "refresh should leave exactly one paragraph"

# Without an upstream source an existing skill keeps its upstream part and
# gets only the block refreshed; a missing skill is skipped, and when nothing
# at all can be written the installer fails.
printf '%s\n' '---' 'name: jevgrep' '---' 'hand-kept upstream part' '' '<!-- harness-jg:start -->' 'old block' '<!-- harness-jg:end -->' > "$CLAUDE_SKILL"
rm -f "$CODEX_SKILL"
"$INSTALL" --home "$HOME_DIR" --skip-global --no-upstream >/dev/null
grep -q 'hand-kept upstream part' "$CLAUDE_SKILL" || fail "existing upstream part was lost without a source"
grep -q 'old block' "$CLAUDE_SKILL" && fail "block should be refreshed without a source"
[ "$(count '<!-- harness-jg:start -->' "$CLAUDE_SKILL")" -eq 1 ] || fail "block-only refresh should leave exactly one block"
[ -f "$CODEX_SKILL" ] && fail "missing skill must not be invented without a source"
rm -f "$CLAUDE_SKILL"
if "$INSTALL" --home "$HOME_DIR" --skip-global --no-upstream >/dev/null 2>&1; then
  fail "installer should fail when no skill can be written"
fi

# Bad arguments fail clearly.
if "$INSTALL" --home "$TMP_ROOT/missing" --source "$UPSTREAM" >/dev/null 2>&1; then
  fail "missing home directory should fail"
fi
if "$INSTALL" --home "$HOME_DIR" --source "$TMP_ROOT/nope.md" >/dev/null 2>&1; then
  fail "missing source file should fail"
fi
if "$INSTALL" --bogus >/dev/null 2>&1; then
  fail "unknown argument should fail"
fi

printf '%s\n' 'PASS: install-jg-skill refreshes both skills and the global guides idempotently'
