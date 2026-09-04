#!/usr/bin/env sh
set -eu

# Claude Code PreToolUse hook for Write, Edit, MultiEdit, NotebookEdit, and Bash.
# In order, it:
#   1. lets pure harness commands through (scripts/harness ..., scripts/action.sh
#      validate ...), except the human-only ones: continue, abort, and
#      knowledge-trust approve;
#   2. applies the denylist (scripts/permit.sh) to the real command or write path;
#   3. refuses to work while a knowledge/ folder is present but not approved;
#   4. requires an active, unpaused harness phase;
#   5. counts the call as one harness step, so the step budget is measured.
# Exit 0 allows the call. Exit 2 blocks it and returns stderr to the agent.
# A human can disable it for a session by exporting HARNESS_HOOK_DISABLE=1 in
# the environment; the denylist refuses that string inside agent commands.

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

payload=$(cat)
if has_cmd python3; then
  parsed=$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("\t\t\t")
    sys.exit(0)
tool = data.get("tool_name", "")
tool_input = data.get("tool_input") if isinstance(data.get("tool_input"), dict) else {}
command = str(tool_input.get("command", "")).replace("\n", " ")
path = str(tool_input.get("file_path") or tool_input.get("notebook_path") or "")
cwd = str(data.get("cwd", ""))
print("\t".join([tool, command, path, cwd]))
')
elif has_cmd node; then
  parsed=$(printf '%s' "$payload" | node -e '
let raw = "";
process.stdin.on("data", (chunk) => { raw += chunk; });
process.stdin.on("end", () => {
  let data;
  try { data = JSON.parse(raw); } catch (error) { process.stdout.write("\t\t\t\n"); return; }
  const input = data.tool_input && typeof data.tool_input === "object" ? data.tool_input : {};
  const command = String(input.command || "").replace(/\n/g, " ");
  const path = String(input.file_path || input.notebook_path || "");
  process.stdout.write([data.tool_name || "", command, path, data.cwd || ""].join("\t") + "\n");
});
')
else
  block "python3 or node is required to read the hook payload."
fi

tab=$(printf '\t')
tool_name=${parsed%%"$tab"*}
rest=${parsed#*"$tab"}
command=${rest%%"$tab"*}
rest=${rest#*"$tab"}
path=${rest%%"$tab"*}
cwd=${rest#*"$tab"}

if [ -z "$tool_name" ]; then
  block "hook payload could not be read; refusing to guess."
fi

# 1. Pure harness commands, with the human-only ones refused.
if [ "$tool_name" = "Bash" ]; then
  stripped=$(printf '%s' "$command" | sed -E 's/^[[:space:]]*cd[[:space:]]+[^;&|]+(&&|;)[[:space:]]*//')
  case "$stripped" in
    *';'*|*'&'*|*'|'*|*'`'*|*"\$("*|*'>'*|*'<'*) stripped="" ;;
  esac
  case "$stripped" in
    harness\ continue*|*/scripts/harness\ continue*|scripts/harness\ continue*|\
    harness\ abort*|*/scripts/harness\ abort*|scripts/harness\ abort*|\
    *knowledge-trust.sh\ approve*)
      block "continue, abort, and knowledge-trust approve are human decisions. Ask the user to run it, e.g. with the ! prefix: ! scripts/harness continue \"<evaluation note>\"."
      ;;
    harness\ *|harness|*/scripts/harness\ *|*/scripts/harness|scripts/harness\ *|scripts/harness)
      exit 0
      ;;
    scripts/action.sh\ validate\ *|*/scripts/action.sh\ validate\ *)
      exit 0
      ;;
  esac
fi

# 2. Denylist on the real call.
[ -x "$PERMIT" ] || block "scripts/permit.sh is missing or not executable."
case "$tool_name" in
  Bash)
    verdict=$("$PERMIT" check --command "$command" --project "$HARNESS_ROOT" 2>&1) || block "denied command. $verdict"
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

# 3. knowledge/ must be approved by a human before any work proceeds.
[ -x "$TRUST" ] || block "scripts/knowledge-trust.sh is missing or not executable."
trust_out=$("$TRUST" check --project "$HARNESS_ROOT" 2>&1) || block "$trust_out"
if [ -n "$cwd" ] && [ "$cwd" != "$HARNESS_ROOT" ] && [ -d "$cwd/knowledge" ]; then
  trust_out=$("$TRUST" check --project "$cwd" 2>&1) || block "$trust_out"
fi

# 4. An active, unpaused phase.
[ -x "$HARNESS_CLI" ] || block "scripts/harness is missing or not executable at $HARNESS_CLI."
status_out=$(cd "$HARNESS_ROOT" && "$HARNESS_CLI" status 2>&1) || block "harness status failed: $status_out"
run_status=$(printf '%s\n' "$status_out" | sed -n 's/^Run:[[:space:]]*.*(\([a-z]*\))$/\1/p' | head -n 1)
phase=$(printf '%s\n' "$status_out" | sed -n 's/^Phase:[[:space:]]*//p' | head -n 1)
case "$run_status" in
  active) ;;
  paused) block "the harness run is paused on a budget; a human evaluates and runs: scripts/harness continue \"<evaluation note>\"." ;;
  complete) block "the harness run is complete; start a new one with: scripts/harness plan start." ;;
  aborted) block "the harness run was aborted; start a new one with: scripts/harness plan start." ;;
  *) block "no harness run exists for $tool_name. Open a phase first: scripts/harness plan start." ;;
esac
case "$phase" in
  plan|build|review) ;;
  *) block "no phase is active (phase=$phase). Open one first: scripts/harness plan start." ;;
esac

# 5. Count the call as one step; a budget pause here blocks the call.
step_out=$(cd "$HARNESS_ROOT" && "$HARNESS_CLI" step --note "tool:$tool_name" 2>&1) && exit 0
step_status=$?
if [ "$step_status" -eq 3 ]; then
  block "step budget reached; the run is paused. $step_out"
fi
block "harness step failed (exit $step_status): $step_out"
