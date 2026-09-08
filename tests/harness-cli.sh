#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CLI="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-cli.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# The CLI reports physical paths, so compare against the resolved temp root.
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)

FIXTURE="$TMP_ROOT/fixture"
WORKDIR="$FIXTURE/src/deep"
OUT="$TMP_ROOT/out.txt"

fail() {
  printf '%s\n' "FAIL: $*"
  if [ -f "$OUT" ]; then
    printf '%s\n' "--- last output ---"
    cat "$OUT"
  fi
  exit 1
}

# Build a fake harness root so discovery, not the real repo state, is exercised.
mkdir -p "$FIXTURE/scripts" "$WORKDIR"
printf '%s\n' '# fixture agent guide' > "$FIXTURE/AGENTS.md"
printf '%s\n' '#!/usr/bin/env sh' > "$FIXTURE/scripts/verify.sh"

# Each case gets its own state directory so runs never leak between cases.
case_number=0
new_case() {
  case_number=$((case_number + 1))
  HARNESS_DB_ROOT="$TMP_ROOT/db-$case_number"
  export HARNESS_DB_ROOT
  unset HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS HARNESS_BUDGET_TOKENS || true
}

# run EXPECTED_EXIT ARGS...  Always executed from a subdirectory of the fixture.
run() {
  expected=$1
  shift
  status=0
  (cd "$WORKDIR" && "$CLI" "$@") > "$OUT" 2>&1 || status=$?
  if [ "$status" -ne "$expected" ]; then
    fail "harness $* exited $status, expected $expected"
  fi
}

expect_output() {
  if ! grep -q -e "$1" "$OUT"; then
    fail "expected output to match: $1"
  fi
}

state_dir() {
  run_id=$(head -n 1 "$HARNESS_DB_ROOT/runs/current")
  printf '%s\n' "$HARNESS_DB_ROOT/runs/$run_id"
}

# record KIND EXIT [EPOCH_OFFSET]  Fake a scripts/verify.sh or scripts/review.sh record.
record() {
  mkdir -p "$HARNESS_DB_ROOT/records"
  epoch=$(( $(date +%s) + ${3:-0} ))
  printf '%s\n' "RECORD_KIND=$1" "RECORD_AT=fixture" "RECORD_EPOCH=$epoch" "GIT_HEAD=fixture" "EXIT=$2" \
    > "$HARNESS_DB_ROOT/records/$1.state"
}

# --- harness root discovery -------------------------------------------------

new_case
run 0 status
expect_output "Harness root: $FIXTURE"
expect_output "Run:          none"

status=0
(cd "$TMP_ROOT" && "$CLI" status) > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "absolute harness path outside its root exited $status, expected 0"
grep -q "Harness root: $HARNESS_ROOT_UNDER_TEST" "$OUT" \
  || fail "absolute harness path should infer its installation root"

# A copied CLI with neither a surrounding harness nor a valid installation root
# still fails with a clear discovery error.
ORPHAN="$TMP_ROOT/orphan"
mkdir -p "$ORPHAN/scripts" "$ORPHAN/work"
cp "$CLI" "$ORPHAN/scripts/harness"
status=0
(cd "$ORPHAN/work" && "$ORPHAN/scripts/harness" status) > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "orphan harness CLI exited $status, expected 2"
grep -q "no harness root found" "$OUT" || fail "expected a clear no-harness-root message"

# --- phase order ------------------------------------------------------------

new_case
run 4 build start
expect_output "no harness run exists"

new_case
run 4 review start
expect_output "no harness run exists"

new_case
run 0 plan start
run 4 build start
expect_output "cannot start build: plan is 'active'"

new_case
run 4 plan "done"
expect_output "no harness run exists"

new_case
run 0 plan start
run 4 review start
expect_output "cannot start review: build is 'pending'"
run 0 plan "done"
run 4 review start
expect_output "cannot start review: build is 'pending'"
run 0 build start
run 4 review start
expect_output "cannot start review: build is 'active'"
record verify 0
run 0 build "done"
expect_output "Gate: verify record"
run 0 review start
record review 0
run 0 review "done"
expect_output "Run complete"
run 0 status
expect_output "Run:.*(complete)"
expect_output "  plan   done"
expect_output "  build  done"
expect_output "  review done"
run 4 build start
expect_output "is complete. Start a new run"

# --- gates: build done needs a fresh passing verify record ------------------

new_case
run 0 plan start
run 0 plan "done"
run 0 build start
run 4 build "done"
expect_output "no verify record"
record verify 1
run 4 build "done"
expect_output "last verify run failed (exit 1)"
record verify 0 -100
run 4 build "done"
expect_output "predates build start"
record verify 0
run 0 build "done"
run 0 review start
run 4 review "done"
expect_output "no review record"
record review 0 -100
run 4 review "done"
expect_output "predates review start"
record review 0
run 0 review "done"

# --- phase done requires an active phase ------------------------------------

new_case
run 0 plan start
run 0 plan "done"
run 4 plan "done"
expect_output "cannot mark plan done: plan is 'done'"

# --- run state lives under .harness-db/runs/<id>/ ---------------------------

new_case
run 0 plan start
RUN_STATE=$(state_dir)
[ -f "$RUN_STATE/state" ] || fail "missing run state file"
[ -f "$RUN_STATE/run.json" ] || fail "missing run.json snapshot"
[ -d "$RUN_STATE/pauses" ] || fail "missing pauses directory"
grep -q '"current_phase": "plan"' "$RUN_STATE/run.json" || fail "run.json does not record the current phase"
grep -q '"steps_used": 1' "$RUN_STATE/run.json" || fail "run.json does not record the step counter"
grep -q '"tokens_used": "unknown"' "$RUN_STATE/run.json" || fail "tokens should be recorded as unknown"
grep -q '"tokens_cap": "unknown"' "$RUN_STATE/run.json" || fail "token cap should be recorded as unknown"

# --- default caps -----------------------------------------------------------

new_case
run 0 plan start
run 0 status
expect_output "steps     1/200"
expect_output "time_min  0/120"
expect_output "loops     0/1"
expect_output "tokens    unknown/unknown"
expect_output "continues 0/3"

# --- step budget: pause and continue ----------------------------------------

new_case
HARNESS_BUDGET_STEPS=3
export HARNESS_BUDGET_STEPS
run 0 plan start
run 0 step --note "read the docs"
run 3 step --note "draft the plan"
expect_output "PAUSE: steps budget reached (used=3 cap=3)"
expect_output "harness continue"

RUN_STATE=$(state_dir)
PAUSE_RECORD="$RUN_STATE/pauses/001.json"
[ -f "$PAUSE_RECORD" ] || fail "missing pause record"
grep -q '"budget": "steps"' "$PAUSE_RECORD" || fail "pause record missing budget"
grep -q '"resolved": false' "$PAUSE_RECORD" || fail "pause record should start unresolved"
grep -q '"evaluation": null' "$PAUSE_RECORD" || fail "pause record should start without an evaluation"

# A paused run refuses further work.
run 3 step --note "keep going anyway"
expect_output "is paused on the steps budget"
run 3 build start
expect_output "is paused on the steps budget"

# Status still works while paused.
run 0 status
expect_output "Paused on:    steps"

# Continue requires an evaluation note.
run 2 continue
expect_output "continue requires an evaluation note"
run 2 continue ""
expect_output "continue requires an evaluation note"

run 0 continue "Plan is still too broad; narrowing to one file before building."
expect_output "resumed"
expect_output "Budget steps extended to 6"
grep -q '"resolved": true' "$PAUSE_RECORD" || fail "pause record should be resolved after continue"
grep -q 'narrowing to one file' "$PAUSE_RECORD" || fail "pause record should store the evaluation note"

# Work resumes after the evaluation, and the extended cap pauses again later.
run 0 step --note "narrow the plan"
run 0 plan "done"
run 0 status
expect_output "steps     5/6"
run 3 build start
expect_output "PAUSE: steps budget reached (used=6 cap=6)"
[ -f "$RUN_STATE/pauses/002.json" ] || fail "missing second pause record"

# Continue on a run that is not paused is refused.
new_case
run 0 plan start
run 4 continue "nothing to evaluate"
expect_output "is not paused"

# --- time budget ------------------------------------------------------------

new_case
HARNESS_BUDGET_TIME_MIN=0
export HARNESS_BUDGET_TIME_MIN
run 3 plan start
expect_output "PAUSE: time_min budget reached (used=0 cap=0)"
run 0 continue "Elapsed time cap was set to zero for this check; extending one minute."
expect_output "Budget time_min extended to 1"
run 0 step --note "work within the extended window"

# A run that is hours over its cap resumes cleanly because continue restarts the clock.
new_case
run 0 plan start
RUN_STATE=$(state_dir)
old_epoch=$(( $(date +%s) - 36000 ))
sed -i.bak "s/^RUN_STARTED_EPOCH=.*/RUN_STARTED_EPOCH=$old_epoch/" "$RUN_STATE/state" && rm -f "$RUN_STATE/state.bak"
run 3 step --note "trips the time cap"
expect_output "PAUSE: time_min budget reached (used=600 cap=120)"
run 0 continue "Resuming after a long break."
run 0 step --note "clock restarted"
run 0 status
expect_output "time_min  0/240"

# --- loop budget ------------------------------------------------------------

new_case
run 0 plan start
run 0 plan "done"
run 0 build start
record verify 0
run 0 build "done"
run 3 plan start
expect_output "LOOP: re-entering a completed plan phase"
expect_output "PAUSE: loops budget reached (used=1 cap=1)"
run 0 continue "Re-planning once because the build uncovered a missing acceptance criterion."
expect_output "Budget loops extended to 2"
run 0 status
expect_output "loops     1/2"

# --- continue cap -----------------------------------------------------------

new_case
HARNESS_BUDGET_STEPS=1
HARNESS_BUDGET_CONTINUES=1
export HARNESS_BUDGET_STEPS HARNESS_BUDGET_CONTINUES
run 3 plan start
run 0 continue "first evaluation"
expect_output "continue 1/1"
run 3 step --note "uses the extended window"
run 3 continue "second evaluation"
expect_output "continue cap reached (1/1)"
expect_output "harness abort"
run 3 step --note "still paused"

# --- abort ------------------------------------------------------------------

new_case
run 4 abort "nothing to abort"
expect_output "no harness run exists"
run 0 plan start
run 2 abort
expect_output "abort requires a reason"
run 0 abort "Scope was wrong; starting over."
expect_output "aborted"
RUN_STATE=$(state_dir)
grep -q 'Scope was wrong' "$RUN_STATE/abort.note" || fail "abort reason not stored"
grep -q '"status": "aborted"' "$RUN_STATE/run.json" || fail "run.json should show aborted"
run 4 build start
expect_output "is aborted. Start a new run"
run 4 abort "again"
expect_output "already aborted"
run 0 plan start
expect_output "Run created"

# --- token accounting -------------------------------------------------------

new_case
run 0 plan start
run 0 step --tokens 1200
run 0 status
expect_output "tokens    1200/unknown"
run 2 step --tokens abc
expect_output "--tokens must be a non-negative integer"

new_case
HARNESS_BUDGET_TOKENS=1500
export HARNESS_BUDGET_TOKENS
run 0 plan start
run 3 step --tokens 1500
expect_output "PAUSE: tokens budget reached (used=1500 cap=1500)"

# --- usage ------------------------------------------------------------------

new_case
run 2
expect_output "Usage: harness COMMAND"
run 0 --help
expect_output "Usage: harness COMMAND"
run 2 plan
expect_output "plan requires 'start' or 'done'"
run 2 plan finish
expect_output "unknown plan action: finish"
run 2 bogus
expect_output "unknown command: bogus"

printf '%s\n' 'PASS: harness CLI phase order, budgets, pause, continue cap, abort, and gates'
