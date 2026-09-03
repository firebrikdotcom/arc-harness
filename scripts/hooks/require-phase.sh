#!/usr/bin/env sh
set -eu

# Claude Code PreToolUse hook. Blocks Write, Edit, and Bash tool calls unless
# the harness CLI reports an active, unpaused phase. Bash calls that only
# drive the harness itself (scripts/harness, scripts/action.sh validate) are
# allowed so the agent can open a phase, except `continue` and `abort`, which
# are budget decisions reserved for a human (run them with the `!` prefix).
# Every allowed call is counted as one harness step, so the step budget is
# measured rather than self-reported.
#
# Exit 0 allows the call. Exit 2 blocks it and returns stderr to the agent.

HOOK_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$HOOK_DIR/../.." && pwd -P)
HARNESS_CLI="$HARNESS_ROOT/scripts/harness"

block() {
  printf '%s\n' "HARNESS BLOCK: $*" >&2
  printf '%s\n' "Open a phase first: scripts/harness plan start (or build/review start), then retry." >&2
  exit 2
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

# Read the hook payload once and extract tool_name and tool_input.command.
payload=$(cat)
if has_cmd python3; then
  parsed=$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("\t")
    sys.exit(0)
tool = data.get("tool_name", "")
command = ""
tool_input = data.get("tool_input")
if isinstance(tool_input, dict):
    command = str(tool_input.get("command", "")).replace("\n", " ")
print(tool + "\t" + command)
')
elif has_cmd node; then
  parsed=$(printf '%s' "$payload" | node -e '
let raw = "";
process.stdin.on("data", (chunk) => { raw += chunk; });
process.stdin.on("end", () => {
  let data;
  try { data = JSON.parse(raw); } catch (error) { process.stdout.write("\t\n"); return; }
  const tool = data.tool_name || "";
  const input = data.tool_input && typeof data.tool_input === "object" ? data.tool_input : {};
  const command = String(input.command || "").replace(/\n/g, " ");
  process.stdout.write(tool + "\t" + command + "\n");
});
')
else
  block "python3 or node is required to read the hook payload."
fi

tool_name=${parsed%%	*}
command=${parsed#*	}

if [ -z "$tool_name" ]; then
  block "hook payload could not be read; refusing to guess."
fi

# Bash calls that only operate the harness are always allowed, but only when
# the harness call is the whole command: no chained or substituted commands.
if [ "$tool_name" = "Bash" ]; then
  # Drop a leading "cd PATH &&" or "cd PATH;" so the real command is inspected.
  stripped=$(printf '%s' "$command" | sed -E 's/^[[:space:]]*cd[[:space:]]+[^;&|]+(&&|;)[[:space:]]*//')
  case "$stripped" in
    *';'*|*'&'*|*'|'*|*'`'*|*"\$("*|*'>'*|*'<'*) stripped="" ;;
  esac
  case "$stripped" in
    harness\ continue*|*/scripts/harness\ continue*|scripts/harness\ continue*|\
    harness\ abort*|*/scripts/harness\ abort*|scripts/harness\ abort*)
      block "'harness continue' and 'harness abort' are human decisions. Ask the user to run it, e.g. with the ! prefix: ! scripts/harness continue \"<evaluation note>\"."
      ;;
    harness\ *|harness|*/scripts/harness\ *|*/scripts/harness|scripts/harness\ *|scripts/harness)
      exit 0
      ;;
    scripts/action.sh\ validate\ *|*/scripts/action.sh\ validate\ *)
      exit 0
      ;;
  esac
fi

if [ ! -x "$HARNESS_CLI" ]; then
  block "scripts/harness is missing or not executable at $HARNESS_CLI."
fi

# Ask the harness for the run state. Failures (no root, no runtime) block.
status_out=$(cd "$HARNESS_ROOT" && "$HARNESS_CLI" status 2>&1) || block "harness status failed: $status_out"

run_status=$(printf '%s\n' "$status_out" | sed -n 's/^Run:[[:space:]]*.*(\([a-z]*\))$/\1/p' | head -n 1)
phase=$(printf '%s\n' "$status_out" | sed -n 's/^Phase:[[:space:]]*//p' | head -n 1)

case "$run_status" in
  active) ;;
  paused) block "the harness run is paused on a budget; evaluate and run: scripts/harness continue \"<evaluation note>\"." ;;
  complete) block "the harness run is complete; start a new one with: scripts/harness plan start." ;;
  *) block "no harness run exists for $tool_name." ;;
esac

case "$phase" in
  plan|build|review) ;;
  *) block "no phase is active (phase=$phase)." ;;
esac

# Count this tool call as one step. A budget pause here blocks the call.
step_out=$(cd "$HARNESS_ROOT" && "$HARNESS_CLI" step --note "tool:$tool_name" 2>&1) && exit 0
step_status=$?
if [ "$step_status" -eq 3 ]; then
  block "step budget reached; the run is paused. $step_out"
fi
block "harness step failed (exit $step_status): $step_out"
