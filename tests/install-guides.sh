#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INSTALL="$HARNESS_ROOT_UNDER_TEST/scripts/install-guides.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-guides.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
PROJECT="$TMP_ROOT/project"
mkdir -p "$PROJECT"
# External blocks name the harness database; start from its default location.
unset HARNESS_DB_ROOT

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
grep -q 'plan start --task TASK.md' "$PROJECT/AGENTS.md" || fail "block missing the plan command with its task"
# shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
grep -q 'criteria as a list under `## Acceptance Criteria`' "$PROJECT/AGENTS.md" || fail "block must say where the task's criteria go"
grep -q 'launch --state TASK.json' "$PROJECT/AGENTS.md" || fail "block missing the task-entry launch command"

grep -q 'advise --context CHECKPOINT.json' "$PROJECT/AGENTS.md" || fail "checkpoint command missing"
grep -q 'advise --record OUTCOME.json' "$PROJECT/CLAUDE.md" || fail "outcome command missing"
grep -q 'shadow mode' "$PROJECT/AGENTS.md" || fail "shadow boundary missing"

# Jev guidance names concrete trigger points instead of a generic paragraph,
# and every trigger command is an exact, runnable flag-form checkpoint.
if grep -q 'Consider Jev at every meaningful decision' "$PROJECT/AGENTS.md"; then
  fail "generic Jev paragraph should be replaced by trigger points"
fi
for family in tool_selection evidence_assessment handoff_assessment; do
  grep -q "^- Before .*advise --family $family " "$PROJECT/AGENTS.md" || fail "trigger for $family missing"
done
# shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
grep -q 'set `--baseline` to the choice you would make without asking' "$PROJECT/AGENTS.md" \
  || fail "trigger commands must tell the agent to substitute its own baseline and facts"
# shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
grep '^- Before' "$PROJECT/AGENTS.md" | sed 's/^[^`]*`//; s/`$//' > "$TMP_ROOT/triggers"
[ "$(wc -l < "$TMP_ROOT/triggers")" -eq 3 ] || fail "expected exactly three trigger commands"
if grep -q "$PROJECT\|$HARNESS_ROOT_UNDER_TEST/scripts/jg" "$TMP_ROOT/triggers"; then
  fail "trigger choices must not embed paths that would be sent to Jev"
fi
(
  export HARNESS_TYPESAFE_ROUTER="$HARNESS_ROOT_UNDER_TEST/tests/fake_router.py"
  export HARNESS_DB_ROOT="$TMP_ROOT/db" FAKE_ROUTER_LOG_DIR="$TMP_ROOT/logs"
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
# The harness root's own guides carry the current block with repository-relative links.
for guide in AGENTS.md CLAUDE.md; do
  grep -qF "plan start --task TASK.md" "$HARNESS_ROOT_UNDER_TEST/$guide" \
    || fail "$guide block is stale; rerun scripts/install-guides.sh --project ."
  grep -qF "Formats: docs/jev-checkpoints.md." "$HARNESS_ROOT_UNDER_TEST/$guide" \
    || fail "$guide block is stale or uses a machine-specific docs path; rerun scripts/install-guides.sh --project ."
done
grep -qF "Formats: $HARNESS_ROOT_UNDER_TEST/docs/jev-checkpoints.md." "$PROJECT/AGENTS.md" \
  || fail "external project block should point at the harness checkpoint docs"
if grep -q 'HARNESS_ROOT=' "$PROJECT/AGENTS.md"; then
  fail "external project block should not repeat HARNESS_ROOT"
fi

# The task step names files that exist from the external project: the harness's
# own template and task database, never paths relative to the project.
for guide in AGENTS.md CLAUDE.md; do
  grep -qF "Write \`TASK.md\` from \`$HARNESS_ROOT_UNDER_TEST/tasks/task-template.md\`" "$PROJECT/$guide" \
    || fail "external $guide block should name the harness task template by absolute path"
  grep -qF "outside this project, at \`$HARNESS_ROOT_UNDER_TEST/.harness-db/tasks/\`" "$PROJECT/$guide" \
    || fail "external $guide block should name the harness task database by absolute path"
  grep -qF "run $HARNESS_ROOT_UNDER_TEST/scripts/review.sh, answer each acceptance criterion" "$PROJECT/$guide" \
    || fail "external $guide block should name the review script by absolute path"
  # shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
  if grep -qF -e '`tasks/task-template.md`' -e '`.harness-db/' "$PROJECT/$guide"; then
    fail "external $guide block must not name project-relative task paths"
  fi
done
[ -f "$HARNESS_ROOT_UNDER_TEST/tasks/task-template.md" ] || fail "the named task template does not exist"
[ ! -e "$PROJECT/tasks" ] || fail "install-guides must not create a tasks directory in the project"
# A custom harness database is the one named.
mkdir -p "$TMP_ROOT/project-db"
HARNESS_DB_ROOT="$TMP_ROOT/custom-db" "$INSTALL" --project "$TMP_ROOT/project-db" >/dev/null
grep -qF "outside this project, at \`$TMP_ROOT/custom-db/tasks/\`" "$TMP_ROOT/project-db/AGENTS.md" \
  || fail "external block should name HARNESS_DB_ROOT when it is set"
# Inside the harness root the same step stays repository-relative.
for guide in AGENTS.md CLAUDE.md; do
  # shellcheck disable=SC2016 # the backticks are literal Markdown delimiters
  grep -qF 'Write `TASK.md` from `tasks/task-template.md` with its criteria as a list under `## Acceptance Criteria`, and keep it in the ignored `.harness-db/tasks/`.' "$HARNESS_ROOT_UNDER_TEST/$guide" \
    || fail "harness root $guide block should keep the relative task paths; rerun scripts/install-guides.sh --project ."
done

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
