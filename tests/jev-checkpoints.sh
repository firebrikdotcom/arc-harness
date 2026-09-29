#!/usr/bin/env sh
# Automatic Jev shadow checkpoints: script behaviour, hook behaviour, and the
# hook installer, all against the offline fake router.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-jev.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*"
  exit 1
}

# count DIR  Number of JSON records directly inside DIR (0 when it is absent).
count() {
  [ -d "$1" ] || { printf '0\n'; return 0; }
  find "$1" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' '
}

python3 -m unittest discover -s tests -p test_phase_checkpoint.py -q
python3 -m unittest discover -s tests -p test_audit_emit.py -q

export HARNESS_TYPESAFE_ROUTER="$ROOT/tests/fake_router.py"
export FAKE_ROUTER_LOG_DIR="$TMP_ROOT/logs"
export HARNESS_AUDIT_ENABLED=0
export HARNESS_ROOT="$ROOT"
export HARNESS_DB_ROOT="$TMP_ROOT/db"
unset HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS HARNESS_BUDGET_TOKENS || true

# A harness target: a git worktree whose guide carries the harness block.
TARGET="$TMP_ROOT/target"
mkdir -p "$TARGET"
git -C "$TARGET" init -q
printf '<!-- harness-cli:start -->\n<!-- harness-cli:end -->\n' > "$TARGET/AGENTS.md"
git -C "$TARGET" -c user.name=t -c user.email=t@example.invalid add AGENTS.md
git -C "$TARGET" -c user.name=t -c user.email=t@example.invalid commit -q -m init

payload() {
  printf '{"session_id":"s","hook_event_name":"%s","cwd":"%s","tool_name":"%s","tool_input":{"command":"%s"}}' "$1" "$2" "$3" "$4"
}

# 1. Hooks are silent no-ops unless HARNESS_JEV_CHECKPOINTS=1.
out=$(payload SessionStart "$TARGET" "" "" | HARNESS_JEV_CHECKPOINTS=0 sh scripts/hooks/session-route.sh)
[ -z "$out" ] || fail "session-route.sh printed while disabled: $out"
out=$(payload PreToolUse "$TARGET" Bash "ls" | HARNESS_JEV_CHECKPOINTS=0 sh scripts/hooks/jev-observe.sh)
[ -z "$out" ] || fail "jev-observe.sh printed while disabled: $out"

# 2. SessionStart in a harness target records one shadow route and one context line.
export HARNESS_JEV_CHECKPOINTS=1
out=$(payload SessionStart "$TARGET" "" "" | sh scripts/hooks/session-route.sh)
case "$out" in
  "Jev shadow route for this session: proceed (source typesafe, shadow; existing rules decide). Pilot: 0/30"*) ;;
  *) fail "unexpected session-route output: $out" ;;
esac
[ "$(count "$HARNESS_DB_ROOT/routes")" = "1" ] || fail "expected one route record"

# 3. SessionStart outside a harness target stays silent and writes nothing.
mkdir -p "$TMP_ROOT/plain"
out=$(payload SessionStart "$TMP_ROOT/plain" "" "" | sh scripts/hooks/session-route.sh)
[ -z "$out" ] || fail "session-route.sh printed outside a target: $out"
[ "$(count "$HARNESS_DB_ROOT/routes")" = "1" ] || fail "route written outside a target"

# 4. The observe hook needs an active run; without one it is silent.
payload PreToolUse "$TARGET" Bash "make test" | sh scripts/hooks/jev-observe.sh >/dev/null
[ ! -d "$HARNESS_DB_ROOT/advice" ] || fail "observe hook wrote advice without a run"

# 5. With a run, the third identical command emits one progress checkpoint; harness commands are ignored.
HARNESS_JEV_CHECKPOINTS=0 scripts/harness plan start >/dev/null
for _ in 1 2 3 4; do
  payload PreToolUse "$TARGET" Bash "scripts/harness status" | sh scripts/hooks/jev-observe.sh >/dev/null
done
for _ in 1 2; do
  payload PreToolUse "$TARGET" Bash "make test" | sh scripts/hooks/jev-observe.sh >/dev/null
done
[ ! -d "$HARNESS_DB_ROOT/advice" ] || fail "checkpoint emitted before the third repeat"
payload PreToolUse "$TARGET" Bash "make test" | sh scripts/hooks/jev-observe.sh >/dev/null
[ "$(count "$HARNESS_DB_ROOT/advice")" = "1" ] || fail "expected one progress checkpoint after three repeats"
grep -q '"family": "progress_assessment"' "$HARNESS_DB_ROOT"/advice/*.json || fail "checkpoint family is not progress_assessment"
grep -q 'make test' "$HARNESS_DB_ROOT"/advice/*.json && fail "command text leaked into the checkpoint"
payload PreToolUse "$TARGET" Bash "make test" | sh scripts/hooks/jev-observe.sh >/dev/null
[ "$(count "$HARNESS_DB_ROOT/advice")" = "1" ] || fail "a fourth repeat emitted another checkpoint"
grep -q '"resolver": "tool_repeat"' "$HARNESS_DB_ROOT"/advice-pending/*.json || fail "tool repeat left no pending oracle"

# 6. Phase gates, verify, and review emit checkpoints and label them from the real results.
plan_out=$(scripts/harness plan "done")
printf '%s\n' "$plan_out" | grep -c 'Jev: handoff_assessment/phase-plan-2 shadow recommendation' >/dev/null || fail "plan done emitted no checkpoint: $plan_out"
# The fourth identical command above recurred after the checkpoint: the oracle labels it stuck.
printf '%s\n' "$plan_out" | grep -c 'Jev: labeled progress_assessment/tool-repeat-2 under_escalated' >/dev/null || fail "plan done did not label the tool repeat: $plan_out"
build_out=$(scripts/harness build start)
printf '%s\n' "$build_out" | grep -c 'Jev: reasoning_allocation/phase-build-2' >/dev/null || fail "build start emitted no checkpoint: $build_out"
[ "$(count "$HARNESS_DB_ROOT/advice-pending")" = "2" ] || fail "expected two pending oracles"
verify_out=$(scripts/verify.sh --project "$TARGET" 2>&1) || fail "verify failed: $verify_out"
printf '%s\n' "$verify_out" | grep -c 'Jev: evidence_assessment/verify-predict-2 shadow recommendation' >/dev/null || fail "verify emitted no prediction"
printf '%s\n' "$verify_out" | grep -c 'Jev: labeled evidence_assessment/verify-predict-2 correct' >/dev/null || fail "verify did not label its prediction"
printf '%s\n' "$verify_out" | grep -c 'Jev: labeled reasoning_allocation/phase-build-2 correct' >/dev/null || fail "verify did not label the allocation"
scripts/harness build "done" >/dev/null
scripts/harness review start >/dev/null
review_out=$(scripts/review.sh --project "$TARGET" 2>&1) || fail "review failed: $review_out"
printf '%s\n' "$review_out" | grep -c 'Jev: handoff_assessment/review-handoff-2 shadow recommendation ready_for_handoff' >/dev/null || fail "review emitted no handoff checkpoint"
done_out=$(scripts/harness review "done")
printf '%s\n' "$done_out" | grep -c 'Jev: labeled handoff_assessment/phase-plan-2 correct' >/dev/null || fail "review done did not label the plan handoff"
printf '%s\n' "$done_out" | grep -c 'Jev: labeled handoff_assessment/review-handoff-2 correct' >/dev/null || fail "review done did not label the review handoff"
printf '%s\n' "$done_out" | grep -c 'Jev: pilot 6/30 labeled shadow decisions' >/dev/null || fail "pilot counter missing: $done_out"
if printf '%s\n' "$done_out" | grep -q 'unlabeled checkpoints'; then fail "a checkpoint was left unlabeled: $done_out"; fi
# One history line from verify.sh, one from the verification inside review.sh.
[ "$(wc -l < "$HARNESS_DB_ROOT/records/verify-history.jsonl" | tr -d ' ')" = "2" ] || fail "expected two verify history lines"
[ "$(count "$HARNESS_DB_ROOT/advice-pending")" = "0" ] || fail "pending oracles remain after run completion"

# 7. Nested harness tests never emit checkpoints even when the outer shell enables them.
HARNESS_DB_ROOT="$TMP_ROOT/nested" scripts/verify.sh --project "$TARGET" >/dev/null 2>&1 || fail "nested verify failed"

# 8. The installer is idempotent and preserves unrelated hooks.
CLAUDE="$TMP_ROOT/claude-settings.json"
CODEX="$TMP_ROOT/codex-hooks.json"
printf '{"model":"x","hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"other-tool"}]}]}}\n' > "$CLAUDE"
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" >/dev/null
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" >/dev/null
python3 - "$CLAUDE" "$CODEX" <<'PY'
import json, sys
claude = json.load(open(sys.argv[1])); codex = json.load(open(sys.argv[2]))
assert claude["model"] == "x"
commands = [h["command"] for g in claude["hooks"]["SessionStart"] for h in g["hooks"]]
assert commands.count("other-tool") == 1, commands
assert sum("auto-init.sh" in c for c in commands) == 1, commands
assert not any("session-route.sh" in c for c in commands), commands
assert sum("jev-observe.sh" in h["command"] for g in claude["hooks"]["PreToolUse"] for h in g["hooks"]) == 1
assert claude["hooks"]["PreToolUse"][0]["matcher"] == "Bash"
assert sum("auto-init.sh" in h["command"] for g in codex["hooks"]["SessionStart"] for h in g["hooks"]) == 1
PY
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" --uninstall >/dev/null
python3 - "$CLAUDE" "$CODEX" <<'PY'
import json, sys
claude = json.load(open(sys.argv[1])); codex = json.load(open(sys.argv[2]))
assert "PreToolUse" not in claude["hooks"]
assert [h["command"] for g in claude["hooks"]["SessionStart"] for h in g["hooks"]] == ["other-tool"]
assert codex == {"hooks": {}}, codex
PY
ls "$TMP_ROOT"/claude-settings.json.bak-jev-* >/dev/null 2>&1 || fail "installer made no backup"

printf '%s\n' 'PASS: automatic Jev checkpoints, hooks, and installer'
