#!/usr/bin/env sh
set -eu

# Issue agents end to end through the harness-signals CLI, with fake claude,
# codex, herdr and difit: a background Claude job is diagnosed read-only in its
# own worktree, its diff viewer starts and stops, a Herdr job is diagnosed from
# its transcript, and a Codex job is taken over, handed back and continued.
# Real agent binaries on PATH are replaced by shims that record any call; the
# test fails if one ran.
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CRATE="$ROOT/services/harness-signals/Cargo.toml"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  [ -n "${TMP:-}" ] && [ -d "$TMP/db" ] && find "$TMP/db" -name '*.log' -exec tail -n 20 {} + >&2 2>/dev/null
  exit 1
}

CARGO_BIN=$(command -v cargo || true)
[ -n "$CARGO_BIN" ] || CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -x "$CARGO_BIN" ] || fail "cargo is required to test issue agents"
command -v python3 >/dev/null 2>&1 || fail "python3 is required to read JSON in this test"
command -v git >/dev/null 2>&1 || fail "git is required"

"$CARGO_BIN" build --quiet --manifest-path "$CRATE" || fail "harness-signals does not build"
TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/services/harness-signals/target"}
BIN="$TARGET_DIR/debug/harness-signals"
[ -x "$BIN" ] || fail "no harness-signals binary at $BIN"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/issue-agents.XXXXXX")
TMP=$(CDPATH='' cd "$TMP" && pwd -P)
cleanup() {
  # Any fake still running belongs to this test: its command line names $TMP.
  pids=$(ps -axo pid=,command= | awk -v t="$TMP" 'index($0, t) { print $1 }')
  # shellcheck disable=SC2086 # one pid per word
  [ -n "$pids" ] && kill $pids 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

FAKES="$TMP/fakes"
POISON="$TMP/poison"
CALLS="$TMP/calls"
HOME_DIR="$TMP/home"
PROJECT="$TMP/project"
DB="$TMP/db"
mkdir -p "$FAKES" "$POISON" "$CALLS" "$HOME_DIR" "$PROJECT/src/app" "$DB/targets/demo-1/board" "$DB/targets/demo-1/signals"

for name in claude codex herdr npx difit; do
  printf '#!/bin/sh\necho "%s $*" >> "%s/REAL"\nexit 97\n' "$name" "$CALLS" > "$POISON/$name"
  chmod +x "$POISON/$name"
done

# Each fake records "cwd\0arg\0arg..." in $CALLS and reads its mode from $TMP/mode.
# shellcheck disable=SC2016 # expanded by the fake when it runs, not here
RECORD='printf "%s\0" "$(pwd -P)" "$@" > "'"$CALLS"'/$(basename "$0")-$$"
mode=$(cat "'"$TMP"'/mode" 2>/dev/null || true)'
cat > "$FAKES/claude" <<EOF
#!/bin/sh
$RECORD
case "\$mode" in
  sleep) trap 'echo "\$\$ TERM" >> "$CALLS/SIGNALS"; exit 143' TERM INT HUP; sleep 60 ;;
  *) cat "$TMP/claude-reply" ;;
esac
EOF
cat > "$FAKES/codex" <<EOF
#!/bin/sh
$RECORD
echo '{"type":"thread.started","thread_id":"019a0000-fake-thread"}'
case "\$mode" in
  sleep) sleep 60 ;;
  *) cat "$TMP/codex-reply" ;;
esac
EOF
cat > "$FAKES/herdr" <<EOF
#!/bin/sh
# Replies copy the real herdr CLI: {"id":"cli:<group>:<verb>","result":{...,"type":...}};
# failures print {"error":{"code","message"},"id"} to stderr and exit 1.
printf '%s\0' "\$@" > "$CALLS/herdr-\$1-\$2-\$\$"
state="$TMP/herdr-state"; mkdir -p "\$state"
fail() { printf '{"error":{"code":"%s","message":"%s"},"id":"cli:%s"}\n' "\$1" "\$2" "\$3" >&2; exit 1; }
case "\$1 \$2" in
  "tab create")
    while [ \$# -gt 0 ]; do case "\$1" in --cwd) printf '%s' "\$2" > "\$state/cwd" ;; --label) printf '%s' "\$2" > "\$state/label" ;; esac; shift; done
    rm -f "\$state/closed"; l=\$(cat "\$state/label")
    echo "{\"id\":\"cli:tab:create\",\"result\":{\"root_pane\":{\"pane_id\":\"w1:p1\",\"tab_id\":\"w1:t1\",\"workspace_id\":\"w1\"},\"tab\":{\"agent_status\":\"unknown\",\"focused\":false,\"label\":\"\$l\",\"number\":1,\"pane_count\":1,\"tab_id\":\"w1:t1\",\"workspace_id\":\"w1\"},\"type\":\"tab_created\"}}" ;;
  "agent start")
    [ -e "\$state/startfail" ] && fail agent_start_failed "the agent did not start" agent:start
    while [ \$# -gt 0 ]; do [ "\$1" = --session-id ] && printf '%s' "\$2" > "\$state/session"; shift; done
    echo '{"id":"cli:agent:start","result":{"type":"agent_started"}}' ;;
  "agent prompt")
    if [ -e "\$state/busy" ]; then
      echo working > "\$state/status"
      while [ -e "\$state/busy" ]; do sleep 0.1; done
      exit 1
    fi
    s=\$(cat "\$state/session"); b=\$(sed 's/[^A-Za-z0-9]/-/g' "\$state/cwd")
    mkdir -p "$HOME_DIR/.claude/projects/\$b"
    cp "$TMP/claude-transcript" "$HOME_DIR/.claude/projects/\$b/\$s.jsonl"
    echo '{"id":"cli:agent:prompt","result":{"type":"agent_prompted"}}' ;;
  "agent get")
    [ -e "\$state/closed" ] && fail agent_not_found "agent target \$3 not found" agent:get
    s=\$(cat "\$state/session" 2>/dev/null || true); st=\$(cat "\$state/status" 2>/dev/null || echo done); c=\$(cat "\$state/cwd" 2>/dev/null || true)
    session=""; [ -n "\$s" ] && session="\"agent_session\":{\"agent\":\"claude\",\"kind\":\"id\",\"source\":\"herdr:claude\",\"value\":\"\$s\"},"
    echo "{\"id\":\"cli:agent:get\",\"result\":{\"agent\":{\"agent\":\"claude\",\$session\"agent_status\":\"\$st\",\"completion_seq\":3,\"cwd\":\"\$c\",\"focused\":false,\"foreground_cwd\":\"\$c\",\"interactive_ready\":true,\"name\":\"\$3\",\"pane_id\":\"w1:p1\",\"revision\":2,\"state_change_seq\":3,\"tab_id\":\"w1:t1\",\"terminal_id\":\"term_1\",\"terminal_title\":\"claude\",\"terminal_title_stripped\":\"claude\",\"workspace_id\":\"w1\"},\"type\":\"agent_info\"}}" ;;
  "agent send-keys")
    echo idle > "\$state/status"; echo '{"id":"cli:agent:send_keys","result":{"type":"keys_sent"}}' ;;
  "agent wait") echo '{"id":"cli:agent:wait","result":{"type":"agent_waited"}}' ;;
  "tab close")
    touch "\$state/closed"; echo '{"id":"cli:tab:close","result":{"type":"tab_closed"}}' ;;
  "pane process-info")
    [ -e "\$state/closed" ] && fail pane_not_found "pane not found" pane:process_info
    echo '{"id":"cli:pane:process_info","result":{"process_info":{"foreground_process_group_id":4242,"foreground_processes":[{"argv":["claude"],"argv0":"claude","cmdline":"claude","cwd":"/tmp","name":"claude","pid":4242}],"pane_id":"w1:p1","shell_pid":4241},"type":"pane_process_info"}}' ;;
  *) fail unknown_command "unexpected call" "\$1:\$2" ;;
esac
EOF
cat > "$FAKES/fake-difit" <<EOF
#!/bin/sh
$RECORD
sleep 60
EOF
chmod +x "$FAKES/claude" "$FAKES/codex" "$FAKES/herdr" "$FAKES/fake-difit"

python3 - "$TMP" <<'PY'
import json, sys
tmp = sys.argv[1]
good = {"likely_cause": "the plan is null for archived plans", "code_involved": "src/app/Billing.php:3",
        "reproduce": "renew an archived plan", "suggested_fix": "load archived plans too",
        "blast_radius": "renewals only", "confidence": "medium"}
reply = "Done.\n<<<ISSUE_DIAGNOSIS_JSON>>>\n" + json.dumps(good) + "\n<<<END_ISSUE_DIAGNOSIS_JSON>>>"
open(f"{tmp}/claude-reply", "w").write(json.dumps({"type": "result", "subtype": "success", "result": reply}) + "\n")
open(f"{tmp}/codex-reply", "w").write(json.dumps({"type": "item.completed", "item": {"type": "agent_message", "text": reply}}) + "\n")
open(f"{tmp}/claude-transcript", "w").write(
    json.dumps({"type": "user", "message": {"role": "user", "content": "<<<ISSUE_DIAGNOSIS_JSON>>> in the prompt"}}) + "\n"
    + json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": reply}]}}) + "\n")
def issue(key, line):
    return {"key": key, "source": "test", "id": key, "title": "Boom " + key, "level": "error", "status": "open",
            "count": 3, "users": 1, "first_seen": None, "last_seen": None, "url": "https://errors.example/i?token=x",
            "frames": [{"path": "/var/www/html/src/app/Billing.php", "line": line, "in_app": True}]}
json.dump({k: issue(k, n) for k, n in (("test#1", 3), ("test#2", 4), ("test#3", 5), ("test#4", 6))},
          open(f"{tmp}/db/targets/demo-1/signals/issues.json", "w"))
json.dump({"default_branch": "main", "nodes": [{"id": "app", "title": "App", "owns": ["src/app/"]}]},
          open(f"{tmp}/db/targets/demo-1/board/board.json", "w"))
json.dump({"projects": [{"name": "demo", "target": "demo-1", "path_map": [{"from": "/var/www/html/", "to": ""}],
                         "providers": [{"type": "command", "argv": ["true"]}]}]},
          open(f"{tmp}/signals.json", "w"))
PY

cat > "$PROJECT/src/app/Billing.php" <<'PHP'
<?php

function renew($plan) {
    return $plan->price;
}
PHP
git -C "$PROJECT" init -q -b main
git -C "$PROJECT" add .
git -C "$PROJECT" -c user.name=t -c user.email=t@t -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -q -m init
printf 'TARGET_ID=demo-1\nTARGET_ROOT=%s\n' "$PROJECT" > "$DB/targets/demo-1/target.state"
PROJECT_STATE=$(git -C "$PROJECT" rev-parse HEAD; git -C "$PROJECT" status --porcelain --untracked-files=all)

export PATH="$POISON:$PATH"
export HARNESS_ISSUE_AGENTS_TEST_MODE=1
export HARNESS_ISSUE_AGENTS_CLAUDE_ARGV="[\"$FAKES/claude\"]"
export HARNESS_ISSUE_AGENTS_CODEX_ARGV="[\"$FAKES/codex\"]"
export HARNESS_ISSUE_AGENTS_DIFIT_ARGV="[\"$FAKES/fake-difit\"]"
export HARNESS_ISSUE_AGENTS_HOME="$HOME_DIR"
export HARNESS_ISSUE_AGENTS_HEARTBEAT_MS=100
export HARNESS_ISSUE_AGENTS_GRACE_MS=300
export HARNESS_ISSUE_AGENTS_TAKEOVER_WAIT_MS=8000
export HARNESS_ISSUE_AGENTS_STEP_QUIET_MS=200
export HARNESS_ISSUE_AGENTS_DAILY_CAP=20
unset HARNESS_ISSUE_AGENTS_HERDR_ARGV HARNESS_ISSUE_AGENTS_HERDR_WORKSPACE HARNESS_ISSUE_AGENTS_AGENT 2>/dev/null || true

jobs() {
  "$BIN" --config "$TMP/signals.json" --db-root "$DB" jobs "$@"
}

# field JSON PYTHON-EXPRESSION: evaluate against the parsed document `d`.
field() {
  printf '%s' "$1" | python3 -c 'import json, sys; d = json.load(sys.stdin); v = eval(sys.argv[1]); print(json.dumps(v) if isinstance(v, (dict, list, bool)) or v is None else v)' "$2"
}

# wait_for FP PYTHON-CONDITION: poll `jobs show` until the condition holds.
wait_for() {
  i=0
  while [ "$i" -lt 300 ]; do
    shown=$(jobs show --target demo-1 --job "$1")
    [ "$(field "$shown" "$2")" = true ] && return 0
    sleep 0.1
    i=$((i + 1))
  done
  fail "job $1 never reached: $2 (last: $(field "$shown" 'd["job"]["status"], d["job"]["error"]'))"
}

# argv_of PREFIX N: the Nth recorded call of a fake, one argument per line.
# The fakes name their records <fake>-<pid>, so ls output is safe to filter.
argv_of() {
  # shellcheck disable=SC2010 # oldest first needs ls -tr
  file=$(ls -tr "$CALLS" | grep "^$1" | sed -n "${2}p")
  [ -n "$file" ] || fail "no call $2 of $1"
  tr '\0' '\n' < "$CALLS/$file"
}

calls() {
  # shellcheck disable=SC2010 # record names are plain words (see argv_of)
  ls "$CALLS" | grep -c "^$1" || true
}

# 1. Background Claude: analyze queues and kicks the runner, which spawns a detached supervisor.
out=$(jobs analyze --target demo-1 --issue test#1 --card app)
FP1=$(field "$out" 'd["job"]["fingerprint"]')
[ "$(field "$out" 'd["created"]')" = true ] || fail "analyze did not create a job: $out"
again=$(jobs analyze --target demo-1 --issue test#1 --card app)
[ "$(field "$again" 'd["created"]')" = false ] || fail "a second analyze created another job"
wait_for "$FP1" 'd["job"]["status"] == "diagnosed"'
[ "$(field "$shown" 'd["diagnosis"]["sections"][0]["text"]')" = "the plan is null for archived plans" ] || fail "diagnosis not rendered"
WT1=$(field "$shown" 'd["job"]["worktree"]["path"]')
SID1=$(field "$shown" 'd["job"]["session_id"]')
argv=$(argv_of claude 1)
[ "$(printf '%s\n' "$argv" | sed -n 1p)" = "$(CDPATH='' cd "$WT1" && pwd -P)" ] || fail "claude did not run in the job worktree"
for flag in --session-id "$SID1" --tools Read,Grep,Glob --permission-mode dontAsk --strict-mcp-config --mcp-config '{"mcpServers":{}}' --setting-sources -p; do
  printf '%s\n' "$argv" | grep -qxF -- "$flag" || fail "claude was launched without $flag"
done
printf '%s\n' "$argv" | grep -q 'token=x' && fail "the packet carries a query string"
[ -z "$(git -C "$WT1" status --porcelain --untracked-files=all)" ] || fail "the worktree changed"
[ "$(git -C "$WT1" rev-parse HEAD)" = "$(git -C "$PROJECT" rev-parse main)" ] || fail "the worktree is not at the base commit"
[ "$(field "$shown" 'd["job"]["worktree"]["base_commit"]')" = "$(git -C "$PROJECT" rev-parse main)" ] || fail "the job did not record its base commit"
grep -q 'ISSUE_DIAGNOSIS' "$DB/targets/demo-1/signals/jobs/$FP1.json" && fail "the job file holds prompt text"
find "$DB" -name '*.packet-*' | grep -q . && fail "a context packet was left behind"

# 2. Diff viewer: local only, idempotent, stopped on request.
out=$(jobs difit start --target demo-1 --job "$FP1")
PORT=$(field "$out" 'd["job"]["difit"]["port"]')
DPID=$(field "$out" 'd["job"]["difit"]["pid"]')
argv=$(argv_of fake-difit 1)
for flag in --host 127.0.0.1 --port "$PORT" --no-open --keep-alive main; do
  printf '%s\n' "$argv" | grep -qxF -- "$flag" || fail "difit was started without $flag"
done
out=$(jobs difit start --target demo-1 --job "$FP1")
[ "$(field "$out" 'd["job"]["difit"]["pid"]')" = "$DPID" ] || fail "a second difit start launched another viewer"
[ "$(field "$(jobs show --target demo-1 --job "$FP1")" 'd["difit_running"]')" = true ] || fail "difit is not running"
if jobs cleanup --target demo-1 --job "$FP1" >/dev/null; then fail "cleanup ran while the viewer was up"; fi
jobs difit stop --target demo-1 --job "$FP1" >/dev/null
kill -0 "$DPID" 2>/dev/null && fail "difit is still running after stop"
jobs difit stop --target demo-1 --job "$FP1" >/dev/null || fail "stopping a stopped viewer failed"

# 3. Herdr launcher: a tab, the read-only profile, and the diagnosis read from the transcript.
out=$(HARNESS_ISSUE_AGENTS_HERDR_ARGV="[\"$FAKES/herdr\"]" HARNESS_ISSUE_AGENTS_HERDR_WORKSPACE=w1 \
  jobs analyze --target demo-1 --issue test#2 --card app)
FP2=$(field "$out" 'd["job"]["fingerprint"]')
[ "$(field "$out" 'd["job"]["launcher"]')" = herdr ] || fail "the job does not use Herdr"
wait_for "$FP2" 'd["job"]["status"] == "diagnosed"'
[ "$(field "$shown" 'd["job"]["herdr_tab"]["tab_id"]')" = w1:t1 ] || fail "the Herdr tab was not recorded"
[ "$(field "$shown" 'd["resume_commands"]["herdr"]["command"]')" = "herdr agent focus 'issue-$(printf %s "$FP2" | cut -c1-8)'" ] || fail "no Herdr focus command"
argv_of herdr-agent-start 1 | grep -qxF -- --strict-mcp-config || fail "the Herdr agent was started with MCP servers"
argv_of herdr-agent-prompt 1 | grep -qxF -- --wait || fail "herdr agent prompt did not wait"
[ "$(calls claude)" = 1 ] || fail "the Herdr job also ran a background agent"

# 3b. Herdr Stop interrupts the agent and calls it stopped only once it is idle; the tab stays open.
herdr_env() {
  HARNESS_ISSUE_AGENTS_HERDR_ARGV="[\"$FAKES/herdr\"]" HARNESS_ISSUE_AGENTS_HERDR_WORKSPACE=w1 "$@"
}
touch "$TMP/herdr-state/busy"
herdr_env jobs analyze --target demo-1 --issue test#2 --card app --reanalyze >/dev/null
wait_for "$FP2" 'd["job"]["status"] == "running" and d["job"]["herdr_tab"] is not None'
i=0
until [ "$(cat "$TMP/herdr-state/status" 2>/dev/null)" = working ]; do
  i=$((i + 1)); [ "$i" -lt 200 ] || fail "the fake Herdr agent never started working"; sleep 0.1
done
out=$(herdr_env jobs stop --target demo-1 --job "$FP2")
rm -f "$TMP/herdr-state/busy"
[ "$(field "$out" 'd["job"]["status"]')" = stopped ] || fail "the Herdr job was not stopped: $out"
[ "$(field "$out" 'd["job"]["herdr_tab"]["tab_id"]')" = w1:t1 ] || fail "stop closed the tab of an idle agent"
field "$out" 'd["job"]["events"][-1]["summary"]' | grep -qF 'is idle in Herdr tab w1:t1' || fail "the stop event does not say the agent is idle"
argv_of herdr-agent-send 1 | grep -qxF esc || fail "stop did not interrupt the agent with esc"
[ "$(calls herdr-tab-close)" = 0 ] || fail "stop closed a tab"

# 3c. A launch that fails after its Herdr tab exists closes that tab and says so.
touch "$TMP/herdr-state/startfail"
herdr_env jobs analyze --target demo-1 --issue test#2 --card app --reanalyze >/dev/null
wait_for "$FP2" 'd["job"]["status"] == "failed"'
rm -f "$TMP/herdr-state/startfail"
field "$shown" '"\n".join(e["summary"] for e in d["job"]["events"])' | grep -qF 'the launch failed after Herdr tab w1:t1 was created, so the tab was closed' || fail "a failed launch left its Herdr tab open: $shown"
[ "$(field "$shown" 'd["job"].get("herdr_tab") is None')" = true ] || fail "a closed tab is still on the job"
argv_of herdr-tab-close 1 | tr '\n' ' ' | grep -qF 'tab close w1:t1' || fail "the tab was not closed by its id"

# 4. Codex: take over a running job, hand it back, and the runner continues the same thread.
printf sleep > "$TMP/mode"
out=$(HARNESS_ISSUE_AGENTS_AGENT=codex jobs analyze --target demo-1 --issue test#3 --card app)
FP3=$(field "$out" 'd["job"]["fingerprint"]')
wait_for "$FP3" 'd["job"]["session_id"] == "019a0000-fake-thread" and d["job"]["pid"] is not None'
APID=$(field "$shown" 'd["job"]["pid"]')
# The agent's own transcript: its last tool call is answered and it has replied.
ROLLOUT="$HOME_DIR/.codex/sessions/2026/10/25"
mkdir -p "$ROLLOUT"
cat > "$ROLLOUT/rollout-2026-10-25T10-00-00-019a0000-fake-thread.jsonl" <<'JSONL'
{"type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}
JSONL
out=$(jobs takeover --target demo-1 --job "$FP3")
[ "$(field "$out" 'd["job"]["owner"]')" = you ] || fail "takeover did not give the job to the person"
[ "$(field "$out" 'd["job"]["stopping"] is not None')" = true ] || fail "a take-over does not show the job stopping"
field "$out" '[e["summary"] for e in d["job"]["events"] if e["type"] == "takeover"][0]' | grep -qF 'the background agent stops after its current step (at most 8s), then its session is yours to resume' || fail "takeover does not say the agent stops after its current step"
WT3=$(field "$out" 'd["job"]["worktree"]["path"]')
[ "$(field "$out" 'd["resume_commands"]["local"]')" = "codex resume '019a0000-fake-thread' --cd '$WT3'" ] || fail "wrong resume command"
[ "$(field "$(jobs show --target demo-1 --job "$FP3")" 'd["takeover_wait_seconds"]')" = 8 ] || fail "show does not report the takeover wait"
wait_for "$FP3" 'd["job"].get("stopping") is None'
field "$shown" 'd["job"]["events"][-1]["summary"]' | grep -qF 'stopped after its current step' || fail "the agent was not stopped at its step boundary: $shown"
kill -0 "$APID" 2>/dev/null && fail "the taken-over agent is still running"
[ "$(field "$(jobs show --target demo-1 --job "$FP3")" 'd["job"]["status"]')" = running ] || fail "a taken-over job was marked finished"
printf ok > "$TMP/mode"
out=$(jobs handback --target demo-1 --job "$FP3")
[ "$(field "$out" 'd["job"]["owner"] == "runner" and d["job"]["status"] in ("queued", "running", "diagnosed")')" = true ] || fail "hand back did not return the job to the runner: $out"
wait_for "$FP3" 'd["job"]["status"] == "diagnosed"'
argv_of codex 2 | tr '\n' ' ' | grep -qF 'exec resume 019a0000-fake-thread' || fail "the runner did not continue the codex thread"
argv_of codex 1 | tr '\n' ' ' | grep -qF -- '--sandbox read-only -c approval_policy="never" -c mcp_servers={} --json' || fail "codex was not read-only"

# 6. Observation: every runner tick looks at jobs that are yours too, only reading them,
#    and closes the job once its agent is over.
JOBS_DIR="$DB/targets/demo-1/signals/jobs"
# proc_pid FP ROLE: the pid recorded for the job's supervisor or agent, or nothing.
proc_pid() {
  python3 -c 'import json, sys; d = json.load(open(sys.argv[1])); print((d.get(sys.argv[2]) or {}).get("pid") or "")' "$JOBS_DIR/$1.pid" "$2"
}
wait_unsupervised() {
  i=0
  while [ -n "$(proc_pid "$1" supervisor)" ]; do
    i=$((i + 1)); [ "$i" -lt 300 ] || fail "the supervisor of $1 never let go"; sleep 0.1
  done
}
# Herdr calls that change something; observation must add none of them.
herdr_writes() {
  # shellcheck disable=SC2010 # record names are plain words (see argv_of)
  ls "$CALLS" | grep -c -e '^herdr-agent-send' -e '^herdr-tab-close' -e '^herdr-agent-prompt' -e '^herdr-agent-start' -e '^herdr-tab-create' || true
}
# take_over_working_herdr_job: reanalyze FP2 in Herdr, wait for its agent to work, then take it over.
take_over_working_herdr_job() {
  rm -f "$TMP/herdr-state/closed" "$TMP/herdr-state/status"
  touch "$TMP/herdr-state/busy"
  herdr_env jobs analyze --target demo-1 --issue test#2 --card app --reanalyze >/dev/null
  wait_for "$FP2" 'd["job"]["status"] == "running" and d["job"]["session_id"] is not None and d["job"]["herdr_tab"] is not None'
  i=0
  until [ "$(cat "$TMP/herdr-state/status" 2>/dev/null)" = working ]; do
    i=$((i + 1)); [ "$i" -lt 200 ] || fail "the fake Herdr agent never started working"; sleep 0.1
  done
  out=$(herdr_env jobs takeover --target demo-1 --job "$FP2")
  [ "$(field "$out" 'd["job"]["owner"]')" = you ] || fail "the Herdr job was not taken over: $out"
  wait_unsupervised "$FP2"
  rm -f "$TMP/herdr-state/busy"
}

# 6a. A taken-over Herdr job whose tab vanished ends; Herdr only gets reads.
take_over_working_herdr_job
writes=$(herdr_writes)
herdr_env jobs run --once >/dev/null
shown=$(jobs show --target demo-1 --job "$FP2")
[ "$(field "$shown" 'd["job"]["status"]')" = running ] || fail "observation ended a job whose agent still works: $shown"
[ "$(field "$shown" 'd["job"]["observed_at"] is not None')" = true ] || fail "the tick did not record observed_at"
touch "$TMP/herdr-state/closed"
herdr_env jobs run --once >/dev/null
wait_for "$FP2" 'd["job"]["status"] == "ended" and d["job"]["owner"] == "you"'
field "$shown" 'd["job"]["events"][-1]["summary"]' | grep -qF 'its tab is gone' || fail "the ended event does not say how it was seen: $shown"
[ "$(herdr_writes)" = "$writes" ] || fail "observing a taken-over Herdr job did more than read it"

# 6b. A taken-over Herdr job whose transcript holds a diagnosis is diagnosed, still owned by you.
take_over_working_herdr_job
writes=$(herdr_writes)
bucket=$(sed 's/[^A-Za-z0-9]/-/g' "$TMP/herdr-state/cwd")
mkdir -p "$HOME_DIR/.claude/projects/$bucket"
cp "$TMP/claude-transcript" "$HOME_DIR/.claude/projects/$bucket/$(cat "$TMP/herdr-state/session").jsonl"
echo idle > "$TMP/herdr-state/status"
herdr_env jobs run --once >/dev/null
wait_for "$FP2" 'd["job"]["status"] == "diagnosed" and d["job"]["owner"] == "you"'
[ "$(field "$shown" 'd["diagnosis"]["sections"][0]["text"]')" = "the plan is null for archived plans" ] || fail "the harvested diagnosis was not rendered"
[ "$(herdr_writes)" = "$writes" ] || fail "harvesting a diagnosis did more than read Herdr"

# 6c. A background job that is yours with its agent still running is left untouched, never signalled.
printf sleep > "$TMP/mode"
out=$(jobs analyze --target demo-1 --issue test#4 --card app)
FP4=$(field "$out" 'd["job"]["fingerprint"]')
wait_for "$FP4" 'd["job"]["pid"] is not None'
APID4=$(proc_pid "$FP4" agent)
SPID4=$(proc_pid "$FP4" supervisor)
[ -n "$APID4" ] && [ -n "$SPID4" ] || fail "no agent or supervisor recorded for $FP4"
# The person drives this agent now: its supervisor is gone and the job is theirs.
kill -9 "$SPID4"
i=0
while kill -0 "$SPID4" 2>/dev/null; do i=$((i + 1)); [ "$i" -lt 100 ] || fail "the supervisor did not exit"; sleep 0.1; done
python3 - "$JOBS_DIR/$FP4.json" <<'PY'
import json, sys
job = json.load(open(sys.argv[1]))
job["owner"] = "you"
json.dump(job, open(sys.argv[1], "w"), indent=2)
PY
jobs run --once >/dev/null
jobs run --once >/dev/null
shown=$(jobs show --target demo-1 --job "$FP4")
[ "$(field "$shown" 'd["job"]["status"] == "running" and d["job"]["owner"] == "you" and d["job"]["observed_at"] is not None')" = true ] || fail "a live agent that is yours was not left running: $shown"
kill -0 "$APID4" 2>/dev/null || fail "observation stopped a live agent that is yours"
[ ! -e "$CALLS/SIGNALS" ] || fail "observation signalled the agent: $(cat "$CALLS/SIGNALS")"
[ "$(proc_pid "$FP4" agent)" = "$APID4" ] || fail "the agent's process record was dropped"
# Once that agent exits on its own, the next tick ends the job.
kill -9 -- "-$APID4" 2>/dev/null || kill -9 "$APID4"
jobs run --once >/dev/null
wait_for "$FP4" 'd["job"]["status"] == "ended" and d["job"]["owner"] == "you"'
field "$shown" 'd["job"]["events"][-1]["summary"]' | grep -qF "the agent process $APID4 has exited" || fail "the ended event does not name the exited process: $shown"
printf ok > "$TMP/mode"

# 6d. The runner's last tick is exposed for the dashboard's stale-service banner.
[ "$(field "$(jobs list)" 'd["runner"]["ticked_at"] is not None')" = true ] || fail "jobs list does not report the runner's last tick"

# 5. Cleanup removes the worktree and its unchanged branch; the project is untouched.
jobs cleanup --target demo-1 --job "$FP1" >/dev/null || fail "cleanup failed"
[ -e "$WT1" ] && fail "cleanup left the worktree"
[ -z "$(git -C "$PROJECT" branch --list "nw/$(printf %s "$FP1" | cut -c1-12)")" ] || fail "cleanup left the branch"
[ "$(git -C "$PROJECT" rev-parse HEAD; git -C "$PROJECT" status --porcelain --untracked-files=all)" = "$PROJECT_STATE" ] || fail "the project changed"
[ "$(field "$(jobs list --target demo-1)" 'len(d["jobs"])')" = 4 ] || fail "jobs list does not show four jobs"

[ ! -e "$CALLS/REAL" ] || fail "a real agent binary was invoked: $(cat "$CALLS/REAL")"
printf '%s\n' 'PASS: issue agents: background, Herdr and Codex jobs, take over and hand back, observation of taken-over jobs, difit, cleanup, no real agents'
