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

block_words() {
  sed -n '/<!-- harness-cli:start -->/,/<!-- harness-cli:end -->/p' "$1" | wc -w | tr -d ' '
}

# Existing file without a block gets the block appended; missing file is created.
printf '%s\n' '# Agent Guide' '' 'Keep this line.' > "$PROJECT/AGENTS.md"
"$INSTALL" --project "$PROJECT" >/dev/null
[ -f "$PROJECT/CLAUDE.md" ] || fail "CLAUDE.md was not created"
grep -q 'Keep this line.' "$PROJECT/AGENTS.md" || fail "existing AGENTS.md content was lost"
[ "$(count_markers "$PROJECT/AGENTS.md")" -eq 1 ] || fail "AGENTS.md should hold exactly one block"
[ "$(count_markers "$PROJECT/CLAUDE.md")" -eq 1 ] || fail "CLAUDE.md should hold exactly one block"

# The block is a short map of the gated workflow, not an encyclopedia.
for command in "plan start" "contract set" "contract waive" "plan done" "build done" "review submit" "review done" "brief" "continue"; do
  grep -q "$command" "$PROJECT/AGENTS.md" || fail "block missing: $command"
done
[ "$(block_words "$PROJECT/AGENTS.md")" -le 350 ] || fail "the block grew past 350 words ($(block_words "$PROJECT/AGENTS.md")); move detail to docs"
if grep -q 'advise --family' "$PROJECT/AGENTS.md"; then
  fail "Jev trigger commands belong in docs/jev-checkpoints.md, not the always-loaded block"
fi

# The optional Jev triggers stay exact, runnable flag-form checkpoints in the docs.
JEV_DOC="$HARNESS_ROOT_UNDER_TEST/docs/jev-checkpoints.md"
for family in tool_selection evidence_assessment handoff_assessment; do
  grep -q "^- Before .*advise --family $family " "$JEV_DOC" || fail "trigger for $family missing from the Jev docs"
done
# shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
grep -q 'set `--baseline` to the choice you would make without asking' "$JEV_DOC" \
  || fail "trigger commands must tell the agent to substitute its own baseline and facts"
# shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
grep '^- Before' "$JEV_DOC" | sed 's/^[^`]*`//; s/`$//' > "$TMP_ROOT/triggers"
[ "$(wc -l < "$TMP_ROOT/triggers")" -eq 3 ] || fail "expected exactly three trigger commands"
(
  cd "$HARNESS_ROOT_UNDER_TEST"
  export HARNESS_TYPESAFE_ROUTER="$HARNESS_ROOT_UNDER_TEST/tests/fake_router.py"
  export HARNESS_DB_ROOT="$TMP_ROOT/db" FAKE_ROUTER_LOG_DIR="$TMP_ROOT/logs"
  export HARNESS_CONFIG_HOME="$TMP_ROOT/config" TYPESAFE_MODEL=fixture HARNESS_JEV_DELEGATION=off
  while IFS= read -r trigger; do
    eval "$trigger" > "$TMP_ROOT/trigger.out" 2>&1 || fail "trigger command failed: $(cat "$TMP_ROOT/trigger.out")"
    grep -q '"advisory": true' "$TMP_ROOT/trigger.out" || fail "trigger command was not evaluated as advice"
  done < "$TMP_ROOT/triggers"
  call_id=$(sed -n 's/.*"call_id": "\([^"]*\)".*/\1/p' "$TMP_ROOT/trigger.out")
  "$HARNESS_ROOT_UNDER_TEST/scripts/harness" advise --label "$call_id" --outcome correct \
    --action-taken "handed off" --evidence "fixture" >/dev/null 2>&1 || fail "documented label command failed"
) || exit 1

# Outside the harness root the commands use a clean absolute CLI path. The CLI
# infers its root from that path, so no repeated environment assignment is needed.
grep -q "^$HARNESS_ROOT_UNDER_TEST/scripts/harness plan start" "$PROJECT/AGENTS.md" \
  || fail "external project block should point at the harness CLI"
grep -qF "$HARNESS_ROOT_UNDER_TEST/docs/jev-checkpoints.md" "$PROJECT/AGENTS.md" \
  || fail "external project block should point at the harness checkpoint docs"
if grep -q 'HARNESS_ROOT=' "$PROJECT/AGENTS.md"; then
  fail "external project block should not repeat HARNESS_ROOT"
fi
# The harness root's own guide carries the current block with repository-relative links,
# and its CLAUDE.md imports AGENTS.md instead of repeating it.
cp "$HARNESS_ROOT_UNDER_TEST/AGENTS.md" "$TMP_ROOT/self-agents.before"
cp "$HARNESS_ROOT_UNDER_TEST/CLAUDE.md" "$TMP_ROOT/self-claude.before"
sh "$INSTALL" --project "$HARNESS_ROOT_UNDER_TEST" > "$TMP_ROOT/self.out" 2>&1 || fail "install on the harness root failed"
if ! cmp -s "$HARNESS_ROOT_UNDER_TEST/AGENTS.md" "$TMP_ROOT/self-agents.before"; then
  cp "$TMP_ROOT/self-agents.before" "$HARNESS_ROOT_UNDER_TEST/AGENTS.md"
  fail "the harness root AGENTS.md block is stale; rerun scripts/install-guides.sh --project ."
fi
cmp -s "$HARNESS_ROOT_UNDER_TEST/CLAUDE.md" "$TMP_ROOT/self-claude.before" || fail "CLAUDE.md must not change"
grep -q 'skipped: .*CLAUDE.md (imports AGENTS.md)' "$TMP_ROOT/self.out" || fail "a CLAUDE.md importing AGENTS.md should be left alone"

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

printf '%s\n' 'PASS: install-guides keeps a short gated-workflow block; Jev triggers live in the docs'
