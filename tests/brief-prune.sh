#!/usr/bin/env sh
# Durable memory: a new session's brief resumes from the run's own records, and
# retention archives old runs without ever touching a live one.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-brief.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_ROOT HARNESS_RETAIN_RUNS HARNESS_BUDGET_STEPS || true
PROJECT="$TMP_ROOT/project"
mkdir -p "$PROJECT"
OUT="$TMP_ROOT/out.txt"
CLI="$ROOT/scripts/harness"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

h() {
  (cd "$PROJECT" && "$CLI" "$@") > "$OUT" 2>&1 || return $?
}

h brief || fail "brief failed with no runs"
grep -q 'no runs yet' "$OUT" || fail "brief should say there are no runs"

# Session a plans with a contract, records steps, and hits a budget.
printf '%s\n' '{"goal":"Ship the parser fix.","deliverables":["src/p.py"],"non_goals":[],"acceptance":[{"id":"tests","criterion":"tests pass","command":["true"]}]}' > "$TMP_ROOT/task.json"
HARNESS_SESSION_ID=a HARNESS_BUDGET_STEPS=4 h plan start || fail "plan start"
HARNESS_SESSION_ID=a h contract set "$TMP_ROOT/task.json" || fail "contract set"
HARNESS_SESSION_ID=a h step --note "read parser and its tests" || fail "step"
HARNESS_SESSION_ID=a h step --note "decided to split on CRLF" || fail "step"
HARNESS_SESSION_ID=a h step --note "over budget" || :

# The same session resumes with its contract, pause, and recent steps.
HARNESS_SESSION_ID=a h brief || fail "brief failed"
grep -q "^This session's run: .*(paused, phase plan)" "$OUT" || fail "brief should show this session's paused run"
grep -q '^Contract: Ship the parser fix.' "$OUT" || fail "brief should show the contract goal"
grep -q '^Acceptance: tests' "$OUT" || fail "brief should list acceptance ids"
grep -q '^Paused on: steps' "$OUT" || fail "brief should show the pause"
grep -q 'decided to split on CRLF' "$OUT" || fail "brief should show recent steps"
grep -q 'step tool:' "$OUT" && fail "brief should hide per-tool step noise"

# A new session sees the latest run but is told to start its own.
HARNESS_SESSION_ID=b h brief || fail "brief failed"
grep -q '^No run in this session yet (harness plan start). Latest run: .*(paused' "$OUT" || fail "a new session should see the latest run as context only"

# Retention: plan start keeps the newest N runs and never archives a live one.
for n in 1 2 3; do
  HARNESS_SESSION_ID="s$n" h plan start || fail "plan start s$n"
  HARNESS_SESSION_ID="s$n" h contract waive "fixture" || fail "waive"
  HARNESS_SESSION_ID="s$n" h abort "finished fixture" 2>/dev/null || :
  sleep 1
done
runs() { ls -1 "$HARNESS_DB_ROOT/runs" | grep -cE '^[0-9]{8}T'; }
[ "$(runs)" -eq 4 ] || fail "expected 4 runs before pruning, got $(runs)"
h prune --keep 1 --dry-run || fail "dry run failed"
grep -q 'Would archive 2 run' "$OUT" || fail "dry run should count 2 archivable runs (the paused live run is kept)"
[ "$(runs)" -eq 4 ] || fail "a dry run must not archive"
HARNESS_RETAIN_RUNS=1 HARNESS_SESSION_ID=s9 h plan start || fail "plan start with retention"
grep -q 'Retention: archived 3 finished run' "$OUT" || fail "plan start should archive old finished runs"
ls "$HARNESS_DB_ROOT/runs/archive/"*.tar.gz >/dev/null 2>&1 || fail "archived runs should be kept as tarballs"
HARNESS_SESSION_ID=a h status || fail "status a"
grep -q '(paused)' "$OUT" || fail "session a's paused run must survive retention"

printf '%s\n' 'PASS: the brief resumes from durable run state, and retention archives only finished runs'
