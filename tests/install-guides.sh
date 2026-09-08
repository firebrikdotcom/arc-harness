#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INSTALL="$HARNESS_ROOT_UNDER_TEST/scripts/install-guides.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-guides.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
PROJECT="$TMP_ROOT/project"
mkdir -p "$PROJECT"

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

count_markers() {
  grep -c -F '<!-- harness-cli:start -->' "$1" || true
}

# Existing file without a block gets the block appended; missing file is created.
printf '%s\n' '# Agent Guide' '' 'Keep this line.' > "$PROJECT/AGENTS.md"
"$INSTALL" --project "$PROJECT" >/dev/null
[ -f "$PROJECT/CLAUDE.md" ] || fail "CLAUDE.md was not created"
grep -q 'Keep this line.' "$PROJECT/AGENTS.md" || fail "existing AGENTS.md content was lost"
[ "$(count_markers "$PROJECT/AGENTS.md")" -eq 1 ] || fail "AGENTS.md should hold exactly one block"
[ "$(count_markers "$PROJECT/CLAUDE.md")" -eq 1 ] || fail "CLAUDE.md should hold exactly one block"
grep -q 'plan start' "$PROJECT/AGENTS.md" || fail "block missing the plan command"

# Outside the harness root the commands use a clean absolute CLI path. The CLI
# infers its root from that path, so no repeated environment assignment is needed.
grep -q "^$HARNESS_ROOT_UNDER_TEST/scripts/harness plan start" "$PROJECT/AGENTS.md" \
  || fail "external project block should point at the harness CLI"
if grep -q 'HARNESS_ROOT=' "$PROJECT/AGENTS.md"; then
  fail "external project block should not repeat HARNESS_ROOT"
fi

# Second run changes nothing.
cp "$PROJECT/AGENTS.md" "$TMP_ROOT/agents.before"
cp "$PROJECT/CLAUDE.md" "$TMP_ROOT/claude.before"
"$INSTALL" --project "$PROJECT" >/dev/null
cmp -s "$PROJECT/AGENTS.md" "$TMP_ROOT/agents.before" || fail "second run modified AGENTS.md"
cmp -s "$PROJECT/CLAUDE.md" "$TMP_ROOT/claude.before" || fail "second run modified CLAUDE.md"

# A stale block is replaced in place, content after it survives.
printf '%s\n' '# Guide' '<!-- harness-cli:start -->' 'old block text' '<!-- harness-cli:end -->' 'Trailing line.' > "$PROJECT/CLAUDE.md"
"$INSTALL" --project "$PROJECT" >/dev/null
grep -q 'old block text' "$PROJECT/CLAUDE.md" && fail "stale block text should be replaced"
grep -q 'Trailing line.' "$PROJECT/CLAUDE.md" || fail "content after the block was lost"
[ "$(count_markers "$PROJECT/CLAUDE.md")" -eq 1 ] || fail "refresh should leave exactly one block"

# Bad arguments fail clearly.
if "$INSTALL" --project "$TMP_ROOT/missing" >/dev/null 2>&1; then
  fail "missing project directory should fail"
fi
if "$INSTALL" --bogus >/dev/null 2>&1; then
  fail "unknown argument should fail"
fi

printf '%s\n' 'PASS: install-guides creates, appends, and refreshes the harness block'
