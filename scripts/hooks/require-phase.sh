#!/usr/bin/env sh
set -eu

# Claude Code PreToolUse hook for Write, Edit, MultiEdit, NotebookEdit, and Bash.
# In order, it:
#   1. binds to the session in the hook payload, so each session uses its own
#      run and a new session cannot inherit another session's phase;
#   2. lets pure harness commands through (scripts/harness ..., scripts/action.sh
#      validate ...), except the human-only ones: abort and knowledge-trust approve.
#      An agent may invoke continue only after an explicit user instruction in the
#      current conversation; that conversation-level authorization is enforced by
#      the agent instructions, not inspectable from this hook payload;
#   3. refuses a guard older than the installed harness (scripts/guard-version);
#   4. applies the denylist (scripts/permit.sh) to the real command or write path;
#      for shell commands it judges the files the command would write, not the text;
#   5. refuses to work while a knowledge/ folder is present but not approved;
#   6. requires an active, unpaused, non-stale harness phase;
#   7. allows project writes only in the build phase (plan and review may write
#      their own artifacts: progress.md, tasks/, task.json, review-findings.json);
#   8. counts the call as one harness step, so the step budget is measured.
# Exit 0 allows the call. Exit 2 blocks it and returns stderr to the agent.
# A human can disable it for a session by exporting HARNESS_HOOK_DISABLE=1 in
# the environment; the denylist refuses that assignment inside agent commands.

HOOK_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$HOOK_DIR/../.." && pwd -P)
HARNESS_CLI="$HARNESS_ROOT/scripts/harness"
PERMIT="$HARNESS_ROOT/scripts/permit.sh"
TRUST="$HARNESS_ROOT/scripts/knowledge-trust.sh"

if [ "${HARNESS_HOOK_DISABLE:-0}" = "1" ]; then
  exit 0
fi

block() {
  printf '%s\n' "HARNESS BLOCK: $*" >&2
  exit 2
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

# The command keeps its newlines (heredocs, multi-line scripts) for the parser;
# the other fields are single-line.
payload=$(cat)
if has_cmd python3; then
  parsed=$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("\t\t\t\t")
    sys.exit(0)
clean = lambda value: str(value or "").replace("\t", " ").replace("\n", " ")
tool_input = data.get("tool_input") if isinstance(data.get("tool_input"), dict) else {}
path = tool_input.get("file_path") or tool_input.get("notebook_path") or ""
print("\t".join(clean(v) for v in (data.get("tool_name"), path, data.get("cwd"), data.get("session_id"), "x")))
')
  command=$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
tool_input = data.get("tool_input") if isinstance(data.get("tool_input"), dict) else {}
sys.stdout.write(str(tool_input.get("command", "")))
')
elif has_cmd node; then
  parsed=$(printf '%s' "$payload" | node -e '
let raw = "";
process.stdin.on("data", (chunk) => { raw += chunk; });
process.stdin.on("end", () => {
  let data;
  try { data = JSON.parse(raw); } catch (error) { process.stdout.write("\t\t\t\t\n"); return; }
  const input = data.tool_input && typeof data.tool_input === "object" ? data.tool_input : {};
  const clean = (value) => String(value || "").replace(/[\t\n]/g, " ");
  const path = input.file_path || input.notebook_path || "";
  process.stdout.write([data.tool_name, path, data.cwd, data.session_id, "x"].map(clean).join("\t") + "\n");
});
')
  command=$(printf '%s' "$payload" | node -e '
let raw = "";
process.stdin.on("data", (chunk) => { raw += chunk; });
process.stdin.on("end", () => {
  try { const data = JSON.parse(raw); process.stdout.write(String((data.tool_input || {}).command || "")); } catch (error) {}
});
')
else
  block "python3 or node is required to read the hook payload."
fi

tab=$(printf '\t')
nl='
'
tool_name=${parsed%%"$tab"*}
rest=${parsed#*"$tab"}
path=${rest%%"$tab"*}
rest=${rest#*"$tab"}
cwd=${rest%%"$tab"*}
rest=${rest#*"$tab"}
session=${rest%%"$tab"*}

if [ -z "$tool_name" ]; then
  block "hook payload could not be read; refusing to guess."
fi

# 1. The payload's session selects this session's run in every harness call below.
if [ -n "$session" ]; then
  HARNESS_SESSION_ID=$session
  export HARNESS_SESSION_ID
fi

# 2. Pure harness commands, with human-only commands refused. continue is allowed
# only when the agent has an explicit current-conversation user instruction.
if [ "$tool_name" = "Bash" ]; then
  stripped=$(printf '%s' "$command" | sed -E 's/^[[:space:]]*cd[[:space:]]+[^;&|]+(&&|;)[[:space:]]*//')
  case "$stripped" in
    *';'*|*'&'*|*'|'*|*'`'*|*"\$("*|*'>'*|*'<'*|*"$nl"*) stripped="" ;;
  esac
  case "$stripped" in
    harness\ abort*|*/scripts/harness\ abort*|scripts/harness\ abort*|\
    *knowledge-trust.sh\ approve*)
      block "abort and knowledge-trust approve are human decisions. Ask the user to run them, e.g. with the ! prefix."
      ;;
    harness\ failure*|*/scripts/harness\ failure*|scripts/harness\ failure*)
      block "the repeated-failure counter is fed by the post-tool hook, not by tool calls."
      ;;
    harness\ *|harness|*/scripts/harness\ *|*/scripts/harness|scripts/harness\ *|scripts/harness)
      exit 0
      ;;
    scripts/action.sh\ validate\ *|*/scripts/action.sh\ validate\ *)
      exit 0
      ;;
  esac
fi

# 3. A checkout whose guard is older than the installed harness (a stale worktree
# of the harness itself) must update before it works; git stays usable for that.
own_version=$(head -n 1 "$HARNESS_ROOT/scripts/guard-version" 2>/dev/null || printf '0')
installed_root=${HARNESS_HOME:-}
if [ -z "$installed_root" ]; then
  installed_root=$(head -n 1 "${XDG_CONFIG_HOME:-${HOME:-/nonexistent}/.config}/harness/root" 2>/dev/null || :)
fi
if [ -n "$installed_root" ] && [ "$installed_root" != "$HARNESS_ROOT" ] && [ -f "$installed_root/scripts/guard-version" ]; then
  installed_version=$(head -n 1 "$installed_root/scripts/guard-version")
  case "$own_version$installed_version" in
    *[!0-9]*) ;;
    *)
      if [ "$installed_version" -gt "$own_version" ]; then
        case "$command" in
          git\ *) ;;
          *) block "this checkout's guard is version $own_version but the installed harness at $installed_root is version $installed_version. Update this checkout (git merge or rebase) before working in it." ;;
        esac
      fi
      ;;
  esac
fi

# 4. Denylist on the real call: the command's write targets, or the write path.
[ -x "$PERMIT" ] || block "scripts/permit.sh is missing or not executable."
subject=""
case "$tool_name" in
  Bash)
    verdict=$("$PERMIT" check --command "$command" --project "$HARNESS_ROOT" --cwd "${cwd:-$HARNESS_ROOT}" 2>&1) || block "denied command. $verdict"
    ;;
  Write|Edit|MultiEdit|NotebookEdit)
    [ -n "$path" ] || block "$tool_name call carries no file path."
    subject=$path
    case "$subject" in
      "$HARNESS_ROOT"/*) subject=${subject#"$HARNESS_ROOT"/} ;;
      "$cwd"/*) [ -n "$cwd" ] && subject=${subject#"$cwd"/} ;;
    esac
    verdict=$("$PERMIT" check --path "$subject" --project "$HARNESS_ROOT" 2>&1) || block "denied write to $path. $verdict"
    ;;
esac

# 5. knowledge/ must be approved by a human before any work proceeds.
[ -x "$TRUST" ] || block "scripts/knowledge-trust.sh is missing or not executable."
trust_out=$("$TRUST" check --project "$HARNESS_ROOT" 2>&1) || block "$trust_out"
if [ -n "$cwd" ] && [ "$cwd" != "$HARNESS_ROOT" ] && [ -d "$cwd/knowledge" ]; then
  trust_out=$("$TRUST" check --project "$cwd" 2>&1) || block "$trust_out"
fi

# 6. An active, unpaused, non-stale phase.
[ -x "$HARNESS_CLI" ] || block "scripts/harness is missing or not executable at $HARNESS_CLI."
status_out=$(cd "$HARNESS_ROOT" && "$HARNESS_CLI" status 2>&1) || block "harness status failed: $status_out"
run_status=$(printf '%s\n' "$status_out" | sed -n 's/^Run:[[:space:]]*.*(\([a-z]*\))$/\1/p' | head -n 1)
phase=$(printf '%s\n' "$status_out" | sed -n 's/^Phase:[[:space:]]*//p' | head -n 1)
case "$run_status" in
  active) ;;
  paused) block "the harness run is paused ($(printf '%s\n' "$status_out" | sed -n 's/^Paused on:[[:space:]]*//p' | head -n 1)); wait for an explicit user instruction in this conversation, then run: scripts/harness continue \"<evaluation note>\"." ;;
  stale) block "the harness run has been idle too long and is stale; start a new one with: scripts/harness plan start." ;;
  complete) block "the harness run is complete; start a new one with: scripts/harness plan start." ;;
  aborted|expired) block "the harness run was $run_status; start a new one with: scripts/harness plan start." ;;
  *) block "no harness run exists for this session ($tool_name). Open a phase first: scripts/harness plan start." ;;
esac
case "$phase" in
  plan|build|review) ;;
  *) block "no phase is active (phase=$phase). Open one first: scripts/harness plan start." ;;
esac

# 7. Plan and review do not change the project; they may write their own artifacts.
is_phase_artifact() {
  case "$1" in
    progress.md|tasks/*|task.json|*/task.json|review-findings.json|*/review-findings.json) return 0 ;;
  esac
  return 1
}
if [ "$phase" != "build" ]; then
  case "$tool_name" in
    Write|Edit|MultiEdit|NotebookEdit)
      case "$subject" in
        /*) ;;
        *) is_phase_artifact "$subject" || block "the $phase phase does not edit project files ($subject). Close it and open build: scripts/harness $phase done, then scripts/harness build start." ;;
      esac
      ;;
    Bash)
      work_root=$HARNESS_ROOT
      case "${cwd:-$HARNESS_ROOT}" in
        "$HARNESS_ROOT"|"$HARNESS_ROOT"/*) ;;
        *) work_root=$cwd ;;
      esac
      targets=$("$PERMIT" targets --command "$command" --project "$work_root" --cwd "${cwd:-$HARNESS_ROOT}" 2>/dev/null || :)
      old_ifs=$IFS
      IFS=$nl
      for target in $targets; do
        [ "$target" != "?" ] || { IFS=$old_ifs; block "the $phase phase cannot tell what this command writes (it does not parse); simplify it or open build."; }
        is_phase_artifact "$target" || { IFS=$old_ifs; block "the $phase phase does not write project files ($target). Close it and open build: scripts/harness $phase done, then scripts/harness build start."; }
      done
      IFS=$old_ifs
      ;;
  esac
fi

# 8. Count the call as one step; a budget pause here blocks the call.
step_out=$(cd "$HARNESS_ROOT" && "$HARNESS_CLI" step --note "tool:$tool_name" 2>&1) && exit 0
step_status=$?
if [ "$step_status" -eq 3 ]; then
  block "step budget reached; the run is paused. $step_out"
fi
block "harness step failed (exit $step_status): $step_out"
