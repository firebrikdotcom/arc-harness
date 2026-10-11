#!/usr/bin/env sh
# The persisted Jev delegation switch: `harness jev on|off|status`, its file,
# the per-shell override, and what route, launch, the session route, and
# advise do with it, all against the offline fake router.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-jev-switch.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$*"
  exit 1
}

# field JSON KEY  Print one top-level value of a JSON object (null for a missing key).
field() {
  printf '%s' "$1" | python3 -c 'import json, sys; value = json.load(sys.stdin).get(sys.argv[1]); print(json.dumps(value) if not isinstance(value, str) else value)' "$2"
}

# The switch, the database, the router, and the model pin all belong to this temp dir.
export HARNESS_CONFIG_HOME="$TMP_ROOT/config"
export HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_TYPESAFE_ROUTER="$ROOT/tests/fake_router.py"
export FAKE_ROUTER_LOG_DIR="$TMP_ROOT/logs"
export HARNESS_AUDIT_ENABLED=0
export TYPESAFE_MODEL=fixture
unset HARNESS_JEV_DELEGATION HARNESS_TYPESAFE_ACTIVE HARNESS_TYPESAFE_OPERATOR_ACTIVATION HARNESS_TYPESAFE_ROLLOUT_PERCENT || true
unset HARNESS_SESSION_ID CODEX_THREAD_ID CLAUDE_SESSION_ID CLAUDE_CODE_SESSION_ID CLAUDECODE CODEX_SANDBOX || true
SWITCH_FILE="$HARNESS_CONFIG_HOME/jev-delegation.json"

# 1. Off by default; status reads without writing.
out=$(scripts/harness jev status)
case "$out" in
  "Jev delegation: off (shadow mode, from default)"*) ;;
  *) fail "unexpected default status: $out" ;;
esac
[ ! -e "$SWITCH_FILE" ] || fail "status wrote the switch file"

# 2. on persists a private file with the reason and pin; a new process reads it back.
scripts/harness jev on --reason "pilot reviewed" >/dev/null
[ -f "$SWITCH_FILE" ] || fail "jev on wrote no switch file"
mode=$(stat -c '%a' "$SWITCH_FILE" 2>/dev/null || stat -f '%Lp' "$SWITCH_FILE")
[ "$mode" = "600" ] || fail "switch file mode is $mode, expected 600"
state=$(scripts/harness jev status --json)
[ "$(field "$state" enabled)" = "true" ] || fail "switch not on: $state"
[ "$(field "$state" source)" = "file" ] || fail "switch source is not the file: $state"
[ "$(field "$state" model)" = "fixture" ] || fail "switch did not store the pin: $state"
[ "$(field "$state" reason)" = "pilot reviewed" ] || fail "switch did not store the reason: $state"
out=$(scripts/harness jev status)
case "$out" in
  "Jev delegation: on (active mode, from file)"*) ;;
  *) fail "unexpected on status: $out" ;;
esac

# 3. With the switch on, route is active and launch selects Jev's profile.
printf '%s\n' '{"version":1,"task_kind":"change","area":"backend","proposed_action":"start_routine_agent","reversibility":"reversible","uncertainty_reason":"test_gap"}' > "$TMP_ROOT/task.json"
printf '%s\n' '["/bin/sh","-c","echo DEFAULT"]' > "$TMP_ROOT/default.json"
printf '%s\n' '["/bin/sh","-c","echo ROUTINE"]' > "$TMP_ROOT/routine.json"
route=$(scripts/harness route --state "$TMP_ROOT/task.json" --project "$TMP_ROOT")
[ "$(field "$route" routing_mode)" = "active" ] || fail "route is not active with the switch on: $route"
[ "$(field "$route" recommendation)" = "proceed" ] || fail "route did not follow Jev: $route"
[ "$(field "$route" delegation)" = "true" ] || fail "route record does not say it was delegated: $route"
[ "$(field "$route" mode_source)" = "delegation" ] || fail "route mode source is not the switch: $route"
launch=$(scripts/harness launch --state "$TMP_ROOT/task.json" --project "$TMP_ROOT" \
  --default-command "$TMP_ROOT/default.json" --routine-command "$TMP_ROOT/routine.json" --dry-run)
[ "$(field "$launch" profile)" = "routine" ] || fail "launch did not select the routine profile: $launch"
# A real launch also asks the fake router to record an outcome, which it refuses on stderr; only stdout matters here.
out=$(scripts/harness launch --state "$TMP_ROOT/task.json" --project "$TMP_ROOT" \
  --default-command "$TMP_ROOT/default.json" --routine-command "$TMP_ROOT/routine.json" 2>/dev/null)
[ "$out" = "ROUTINE" ] || fail "launch ran '$out', expected ROUTINE"

# 4. The per-shell override and a per-call --mode win over the file.
route=$(HARNESS_JEV_DELEGATION=off scripts/harness route --state "$TMP_ROOT/task.json" --project "$TMP_ROOT")
[ "$(field "$route" routing_mode)" = "shadow" ] || fail "environment override did not force shadow: $route"
[ "$(field "$route" recommendation)" = "default" ] || fail "shadow route changed the recommendation: $route"
out=$(HARNESS_JEV_DELEGATION=off scripts/harness launch --state "$TMP_ROOT/task.json" --project "$TMP_ROOT" \
  --default-command "$TMP_ROOT/default.json" --routine-command "$TMP_ROOT/routine.json" 2>/dev/null)
[ "$out" = "DEFAULT" ] || fail "overridden launch ran '$out', expected DEFAULT"
route=$(scripts/harness route --state "$TMP_ROOT/task.json" --project "$TMP_ROOT" --mode shadow)
[ "$(field "$route" mode_source)" = "flag" ] || fail "--mode did not win over the switch: $route"
out=$(HARNESS_JEV_DELEGATION=off scripts/harness jev status)
case "$out" in
  "Jev delegation: off (shadow mode, from environment)"*) ;;
  *) fail "status did not report the override: $out" ;;
esac

# 5. Deterministic gates decide before any call, switch or not.
printf '%s\n' '{"version":1,"task_kind":"change","area":"backend","proposed_action":"start_routine_agent","reversibility":"reversible","uncertainty_reason":"test_gap","approval_required":true}' > "$TMP_ROOT/gated.json"
route=$(scripts/harness route --state "$TMP_ROOT/gated.json" --project "$TMP_ROOT")
[ "$(field "$route" source)" = "deterministic" ] || fail "authorization was not deterministic: $route"
[ "$(field "$route" recommendation)" = "ask_user" ] || fail "authorization did not stop at the user: $route"

# 6. advise follows Jev's choice with the switch on and keeps the baseline off.
advice=$(FAKE_ROUTER_CHOICE=retrieval scripts/harness advise --family tool_selection --baseline grep \
  --goal "Locate the code for one task." --choice grep="A targeted grep." --choice retrieval="One semantic retrieval first.")
[ "$(field "$advice" action)" = "retrieval" ] || fail "delegated advise did not return Jev's choice: $advice"
[ "$(field "$advice" delegated)" = "true" ] || fail "delegated advise is not marked delegated: $advice"
[ "$(field "$advice" baseline)" = "grep" ] || fail "delegated advise lost the baseline: $advice"
advice=$(FAKE_ROUTER_CHOICE=retrieval HARNESS_JEV_DELEGATION=off scripts/harness advise --family tool_selection --baseline grep \
  --goal "Locate the code for one task." --choice grep="A targeted grep." --choice retrieval="One semantic retrieval first.")
[ "$(field "$advice" action)" = "grep" ] || fail "shadow advise did not keep the baseline: $advice"
[ "$(field "$advice" delegated)" = "false" ] || fail "shadow advise is marked delegated: $advice"

# 7. The session-start route names the delegated route to follow.
TARGET="$TMP_ROOT/target"
mkdir -p "$TARGET"
git -C "$TARGET" init -q
printf '<!-- harness-cli:start -->\n<!-- harness-cli:end -->\n' > "$TARGET/AGENTS.md"
git -C "$TARGET" -c user.name=t -c user.email=t@example.invalid add AGENTS.md
git -C "$TARGET" -c user.name=t -c user.email=t@example.invalid commit -q -m init
out=$(HARNESS_JEV_CHECKPOINTS=1 python3 scripts/phase_checkpoint.py session-start --project "$TARGET" --db-root "$HARNESS_DB_ROOT")
case "$out" in
  "Jev delegated route for this session: proceed (source typesafe, active; follow it unless a deterministic rule decides)."*) ;;
  *) fail "unexpected delegated session line: $out" ;;
esac
out=$(HARNESS_JEV_CHECKPOINTS=1 HARNESS_JEV_DELEGATION=off python3 scripts/phase_checkpoint.py session-start --project "$TARGET" --db-root "$HARNESS_DB_ROOT")
case "$out" in
  "Jev shadow route for this session: proceed (source typesafe, shadow; existing rules decide)."*) ;;
  *) fail "unexpected shadow session line: $out" ;;
esac

# 8. off persists too, and a corrupt file is refused rather than guessed.
scripts/harness jev off --reason "pause the pilot" >/dev/null
[ "$(field "$(scripts/harness jev status --json)" enabled)" = "false" ] || fail "jev off did not persist"
route=$(scripts/harness route --state "$TMP_ROOT/task.json" --project "$TMP_ROOT")
[ "$(field "$route" routing_mode)" = "shadow" ] || fail "route still active after jev off: $route"
printf '%s\n' '{broken' > "$SWITCH_FILE"
if scripts/harness jev status >"$TMP_ROOT/out" 2>&1; then fail "corrupt switch file was accepted"; fi
grep -q 'delegation switch' "$TMP_ROOT/out" || fail "corrupt switch file not named: $(cat "$TMP_ROOT/out")"
if scripts/harness route --state "$TMP_ROOT/task.json" --project "$TMP_ROOT" >"$TMP_ROOT/out" 2>&1; then fail "route accepted a corrupt switch file"; fi
if scripts/harness jev on --model jev-latest >"$TMP_ROOT/out" 2>&1; then fail "a -latest pin was accepted"; fi

printf '%s\n' 'PASS: jev delegation switch'
