#!/usr/bin/env sh
set -eu

# Sessions in one checkout share runs/current. These cases run harness
# commands concurrently and check that the run-state lock serializes them.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CLI="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
unset HARNESS_ROOT HARNESS_LOCK_TIMEOUT
unset HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS HARNESS_BUDGET_TOKENS || true
HARNESS_JEV_CHECKPOINTS=0
export HARNESS_JEV_CHECKPOINTS
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-lock.XXXXXX")
HOLDER=""
cleanup() {
  if [ -n "$HOLDER" ]; then
    kill "$HOLDER" 2>/dev/null || :
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)

FIXTURE="$TMP_ROOT/fixture"
WORKDIR="$FIXTURE/src"
OUT="$TMP_ROOT/out.txt"
PARALLEL=24

fail() {
  printf '%s\n' "FAIL: $*"
  if [ -f "$OUT" ]; then
    printf '%s\n' "--- last output ---"
    cat "$OUT"
  fi
  exit 1
}

mkdir -p "$FIXTURE/scripts" "$WORKDIR"
printf '%s\n' '# fixture agent guide' > "$FIXTURE/AGENTS.md"
printf '%s\n' '#!/usr/bin/env sh' > "$FIXTURE/scripts/verify.sh"

case_number=0
new_case() {
  case_number=$((case_number + 1))
  HARNESS_DB_ROOT="$TMP_ROOT/db-$case_number"
  export HARNESS_DB_ROOT
  LOCK="$HARNESS_DB_ROOT/runs/.lock"
}

run() {
  expected=$1
  shift
  status=0
  (cd "$WORKDIR" && "$CLI" "$@") > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "harness $* exited $status, expected $expected"
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

state_value() {
  run_id=$(head -n 1 "$HARNESS_DB_ROOT/runs/current")
  sed -n "s/^$1=//p" "$HARNESS_DB_ROOT/runs/$run_id/state" | tail -n 1
}

expect_unlocked() {
  if [ -e "$LOCK" ] || [ -L "$LOCK" ]; then
    fail "$1: the run-state lock was left behind ($(readlink "$LOCK" 2>/dev/null || echo not-a-link))"
  fi
}

# A pid that has exited and been reaped, so nothing owns it.
dead_pid() {
  sh -c 'exit 0' &
  _pid=$!
  wait "$_pid"
  printf '%s\n' "$_pid"
}

# --- parallel steps all count -----------------------------------------------

new_case
run 0 plan start
i=0
pids=""
while [ "$i" -lt "$PARALLEL" ]; do
  (cd "$WORKDIR" && "$CLI" step --note "parallel $i") > "$TMP_ROOT/step-$i.txt" 2>&1 &
  pids="$pids $!"
  i=$((i + 1))
done
failed=0
for pid in $pids; do
  wait "$pid" || failed=$((failed + 1))
done
[ "$failed" -eq 0 ] || { cat "$TMP_ROOT"/step-*.txt; fail "$failed of $PARALLEL parallel steps failed"; }
steps=$(state_value STEPS_USED)
[ "$steps" = "$((PARALLEL + 1))" ] || fail "parallel steps lost updates: STEPS_USED=$steps, expected $((PARALLEL + 1))"
run_id=$(head -n 1 "$HARNESS_DB_ROOT/runs/current")
logged=$(grep -c 'step parallel' "$HARNESS_DB_ROOT/runs/$run_id/log")
[ "$logged" -eq "$PARALLEL" ] || fail "run log has $logged parallel steps, expected $PARALLEL"
grep -q "\"steps_used\": $((PARALLEL + 1))" "$HARNESS_DB_ROOT/runs/$run_id/run.json" \
  || fail "run.json does not show the final step count"
expect_unlocked "parallel steps"
leftovers=$(find "$HARNESS_DB_ROOT/runs" -name '*.tmp.*' | wc -l)
[ "$leftovers" -eq 0 ] || fail "temporary state files were left behind"

# --- the pointer and snapshots are replaced by rename -----------------------
# A reader (the phase guard's `status`) must never see a truncated file. An
# in-place rewrite keeps the inode; a rename-replace always changes it.

inode() {
  # shellcheck disable=SC2012 # POSIX ls -i is the portable inode reader; the paths are fixed
  ls -i "$1" | awk '{ print $1 }'
}

new_case
run 0 plan start
run_id=$(head -n 1 "$HARNESS_DB_ROOT/runs/current")
before=$(inode "$HARNESS_DB_ROOT/runs/$run_id/run.json")
run 0 step --note "rewrite the snapshot"
[ "$(inode "$HARNESS_DB_ROOT/runs/$run_id/run.json")" != "$before" ] || fail "run.json was rewritten in place"
run 0 abort "Lock test: replace the pointer."
before=$(inode "$HARNESS_DB_ROOT/runs/current")
run 0 plan start
[ "$(head -n 1 "$HARNESS_DB_ROOT/runs/current")" != "$run_id" ] || fail "plan start did not point at the new run"
[ "$(inode "$HARNESS_DB_ROOT/runs/current")" != "$before" ] || fail "runs/current was rewritten in place"

# --- concurrent plan start opens one run ------------------------------------

new_case
i=0
pids=""
while [ "$i" -lt 6 ]; do
  (cd "$WORKDIR" && "$CLI" plan start) > "$TMP_ROOT/start-$i.txt" 2>&1 &
  pids="$pids $!"
  i=$((i + 1))
done
ok=0
refused=0
for pid in $pids; do
  status=0
  wait "$pid" || status=$?
  case "$status" in
    0) ok=$((ok + 1)) ;;
    4) refused=$((refused + 1)) ;;
    *) cat "$TMP_ROOT"/start-*.txt; fail "concurrent plan start exited $status" ;;
  esac
done
runs=$(find "$HARNESS_DB_ROOT/runs" -mindepth 2 -maxdepth 2 -name state | wc -l)
[ "$runs" -eq 1 ] || fail "concurrent plan start created $runs runs, expected 1"
if [ "$ok" -ne 1 ] || [ "$refused" -ne 5 ]; then
  fail "concurrent plan start: $ok succeeded and $refused were refused, expected 1 and 5"
fi
explained=$(grep -l 'plan is already started' "$TMP_ROOT"/start-*.txt | wc -l)
[ "$explained" -eq 5 ] || fail "refused plan starts should say the phase is already started ($explained of 5 did)"
expect_unlocked "concurrent plan start"

# --- a lock left by a dead process is recovered -----------------------------

new_case
run 0 plan start
ln -s "pid:$(dead_pid)" "$LOCK"
run 0 step --note "after a crashed holder"
[ "$(state_value STEPS_USED)" = "2" ] || fail "step after a stale lock was not recorded"
expect_unlocked "stale lock recovery"

# Many waiters behind one crashed holder: exactly one clears it, and none
# removes the lock of the process that takes it next.
ln -s "pid:$(dead_pid)" "$LOCK"
i=0
pids=""
while [ "$i" -lt "$PARALLEL" ]; do
  (cd "$WORKDIR" && "$CLI" step --note "behind a crash $i") > "$TMP_ROOT/crash-$i.txt" 2>&1 &
  pids="$pids $!"
  i=$((i + 1))
done
failed=0
for pid in $pids; do
  wait "$pid" || failed=$((failed + 1))
done
[ "$failed" -eq 0 ] || { cat "$TMP_ROOT"/crash-*.txt; fail "$failed of $PARALLEL steps behind a crashed holder failed"; }
steps=$(state_value STEPS_USED)
[ "$steps" = "$((PARALLEL + 2))" ] || fail "steps behind a crashed holder lost updates: STEPS_USED=$steps, expected $((PARALLEL + 2))"
expect_unlocked "crashed holder with parallel waiters"
if [ -e "$LOCK.break" ] || [ -L "$LOCK.break" ]; then
  fail "the stale-lock guard was left behind"
fi

# --- a live holder makes commands wait, then time out without changes -------

new_case
run 0 plan start
sleep 30 &
HOLDER=$!
ln -s "pid:$HOLDER" "$LOCK"
started=$(date +%s)
HARNESS_LOCK_TIMEOUT=1 run 2 step --note "blocked"
waited=$(( $(date +%s) - started ))
[ "$waited" -ge 1 ] || fail "a held lock should make the command wait for the timeout"
expect_output "timed out after 1s waiting for the run-state lock"
expect_output "held by pid:$HOLDER"
expect_output "HARNESS_LOCK_TIMEOUT"
[ "$(state_value STEPS_USED)" = "1" ] || fail "a command that timed out must not change the run"
[ "$(readlink "$LOCK")" = "pid:$HOLDER" ] || fail "a waiter must not remove a live holder's lock"

# Status reads without the lock, so the phase guard is never stuck behind it.
HARNESS_LOCK_TIMEOUT=0 run 0 status
expect_output "Run:.*(active)"

# The waiter proceeds once the holder is gone.
kill "$HOLDER"
wait "$HOLDER" 2>/dev/null || :
HOLDER=""
run 0 step --note "holder exited"
[ "$(state_value STEPS_USED)" = "2" ] || fail "step after the holder exited was not recorded"
expect_unlocked "after holder exit"

# A waiter blocked on a live holder picks the lock up as soon as it is released.
sleep 30 &
HOLDER=$!
ln -s "pid:$HOLDER" "$LOCK"
(cd "$WORKDIR" && HARNESS_LOCK_TIMEOUT=20 "$CLI" step --note "waited") > "$OUT" 2>&1 &
waiter=$!
sleep 1
[ "$(state_value STEPS_USED)" = "2" ] || fail "the waiter changed the run while the lock was held"
rm -f "$LOCK"
kill "$HOLDER"
wait "$HOLDER" 2>/dev/null || :
HOLDER=""
wait "$waiter" || fail "the waiting step failed after the lock was released"
[ "$(state_value STEPS_USED)" = "3" ] || fail "the waiting step was not recorded"
expect_unlocked "after a waited step"

# --- a lock the harness did not write is never removed ----------------------

new_case
run 0 plan start
ln -s "not-a-harness-owner" "$LOCK"
HARNESS_LOCK_TIMEOUT=0 run 2 step --note "foreign lock"
expect_output "held by not-a-harness-owner"
expect_output "a person can remove"
[ "$(readlink "$LOCK")" = "not-a-harness-owner" ] || fail "a foreign lock must be left in place"
rm -f "$LOCK"
for owner in "pid:12x" "pid:"; do
  ln -s "$owner" "$LOCK"
  HARNESS_LOCK_TIMEOUT=0 run 2 step --note "malformed owner"
  [ "$(readlink "$LOCK")" = "$owner" ] || fail "a lock with the malformed owner $owner must be left in place"
  rm -f "$LOCK"
done
mkdir "$LOCK"
HARNESS_LOCK_TIMEOUT=0 run 2 step --note "directory in the way"
if [ ! -d "$LOCK" ] || [ -n "$(ls -A "$LOCK")" ]; then
  fail "a directory at the lock path must be left untouched and empty"
fi
rmdir "$LOCK"
run 0 step --note "lock path clear again"

new_case
run 0 plan start
HARNESS_LOCK_TIMEOUT=soon run 2 step --note "bad timeout"
expect_output "HARNESS_LOCK_TIMEOUT must be a non-negative integer"
[ "$(state_value STEPS_USED)" = "1" ] || fail "a bad timeout must not change the run"

# --- the lock is released on every exit path --------------------------------

new_case
run 0 plan start
run 0 plan "done"
run 0 build start
run 4 build "done"
expect_output "no verify record"
expect_unlocked "gate failure"
run 4 plan "done"
expect_unlocked "phase-order failure"
run 2 step --tokens many
expect_unlocked "usage failure"

new_case
HARNESS_BUDGET_STEPS=2
export HARNESS_BUDGET_STEPS
run 0 plan start
run 3 step --note "reaches the cap"
expect_output "PAUSE: steps budget reached"
expect_unlocked "budget pause"
run 0 continue "Lock test: extending once."
expect_unlocked "continue"
unset HARNESS_BUDGET_STEPS

new_case
run 0 plan start
run 0 abort "Lock test abort."
expect_unlocked "abort"

# A command interrupted by a signal while it holds the lock releases it. A
# python3 shim that sleeps keeps the step inside its locked section.
REAL_PYTHON=$(command -v python3 || :)
if [ -n "$REAL_PYTHON" ]; then
  new_case
  run 0 plan start
  SHIM="$TMP_ROOT/slow-bin"
  mkdir -p "$SHIM"
  printf '%s\n' '#!/usr/bin/env sh' 'sleep 2' "exec \"$REAL_PYTHON\" \"\$@\"" > "$SHIM/python3"
  chmod +x "$SHIM/python3"
  (cd "$WORKDIR" && PATH="$SHIM:$PATH" exec "$CLI" step --note "interrupted") > "$OUT" 2>&1 &
  victim=$!
  tries=0
  until [ -L "$LOCK" ]; do
    tries=$((tries + 1))
    [ "$tries" -lt 100 ] || fail "the interrupted step never took the lock"
    sleep 0.1 2>/dev/null || sleep 1
  done
  kill -TERM "$victim"
  status=0
  wait "$victim" || status=$?
  [ "$status" -eq 143 ] || fail "an interrupted step exited $status, expected 143"
  expect_unlocked "signal"
  run 0 step --note "after a signal"
else
  printf '%s\n' 'SKIP: python3 is not available for the interrupted-step case'
fi

printf '%s\n' 'PASS: harness run-state lock serializes concurrent commands, recovers stale locks, and times out clearly'
