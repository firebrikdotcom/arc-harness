#!/usr/bin/env sh
# Register the harness hooks in the user's Claude Code settings and Codex
# hooks file: automatic target initialisation plus the shadow Jev route at
# SessionStart, the repeated-command observer at PreToolUse, and (Claude Code
# only, which has Grep and Glob tools) the retrieval reminder. Idempotent:
# existing harness entries are replaced, other hooks are preserved, and each
# file is backed up before it is rewritten.
#
#   scripts/install-hooks.sh [--claude PATH] [--codex PATH] [--root-file PATH] [--uninstall] [--dry-run]
#
# It also records this checkout as the installed harness root (default
# ~/.config/harness/root); the phase guard refuses to run from a checkout whose
# scripts/guard-version is older than the installed one.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CLAUDE_SETTINGS="${HOME}/.claude/settings.json"
CODEX_HOOKS="${HOME}/.codex/hooks.json"
ROOT_FILE="${XDG_CONFIG_HOME:-${HOME}/.config}/harness/root"
MODE=install
DRY_RUN=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --claude) CLAUDE_SETTINGS=$2; shift 2 ;;
    --codex) CODEX_HOOKS=$2; shift 2 ;;
    --root-file) ROOT_FILE=$2; shift 2 ;;
    --uninstall) MODE=uninstall; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help)
      sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) printf 'FAIL: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

command -v python3 >/dev/null 2>&1 || { printf 'FAIL: python3 is required.\n' >&2; exit 2; }

python3 - "$HARNESS_ROOT" "$MODE" "$DRY_RUN" "$CLAUDE_SETTINGS" "$CODEX_HOOKS" <<'PY'
import json
import sys
import time
from pathlib import Path

root, mode, dry_run, *targets = sys.argv[1:]
dry_run = dry_run == "1"
SESSION = str(Path(root) / "scripts/hooks/auto-init.sh")
OBSERVE = str(Path(root) / "scripts/observe_commands.py")
SESSION_BIND = str(Path(root) / "scripts/session_hook.py")
REMIND = str(Path(root) / "scripts/retrieval-reminder.sh")
WORKFLOW = str(Path(root) / "scripts/workflow_audit.py")
TODO_GATE = str(Path(root) / "scripts/workflow_gate.py")
MARKERS = ("scripts/hooks/auto-init.sh", "scripts/hooks/session-route.sh", "scripts/hooks/jev-observe.sh",
           "scripts/retrieval-reminder.sh", "scripts/workflow_audit.py", "scripts/workflow_gate.py",
           "scripts/session_hook.py", "scripts/observe_commands.py")
SHARED = {
    "SessionStart": [{"matcher": "startup|resume|clear", "hooks": [{"type": "command", "command": f'python3 "{SESSION_BIND}" "{SESSION}"', "timeout": 30}]}],
    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": f'python3 "{OBSERVE}"', "timeout": 15}]}],
}
# Codex has no Grep or Glob tool, so the retrieval reminder goes to Claude Code only.
CLAUDE_ONLY = {
    "PreToolUse": [{"matcher": "Grep|Glob", "hooks": [{"type": "command", "command": f'"{REMIND}"', "timeout": 10}]}],
}


def entries_for(index: int) -> dict:
    extra = CLAUDE_ONLY if index == 0 else {}
    entries = {event: SHARED[event] + extra.get(event, []) for event in SHARED}
    # Use native lifecycle events; Stop is a turn boundary, not a session end.
    events = ["SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"]
    if index == 0:
        events.append("PostToolUseFailure")
    for event in events:
        entry = {"hooks": [{"type": "command", "command": f'python3 "{WORKFLOW}" hook --agent {"claude-code" if index == 0 else "codex"}', "timeout": 3 if event == "SessionEnd" else 10}]}
        if event in ("PreToolUse", "UserPromptSubmit", "Stop"):
            entry["hooks"][0]["command"] = f'python3 "{TODO_GATE}" {"claude-code" if index == 0 else "codex"}'
        if event in ("PreToolUse", "PostToolUse", "PostToolUseFailure"):
            entry["matcher"] = ".*"
        entries.setdefault(event, []).append(entry)
    return entries


def is_ours(group: dict) -> bool:
    for hook in group.get("hooks", []) if isinstance(group, dict) else []:
        command = str(hook.get("command", "")) if isinstance(hook, dict) else ""
        if any(marker in command for marker in MARKERS):
            return True
    return False


for index, target in enumerate(targets):
    path = Path(target)
    data = {}
    if path.is_file():
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            print(f"FAIL: {path} is not valid JSON ({error}); not touching it.", file=sys.stderr)
            sys.exit(1)
    if not isinstance(data, dict):
        print(f"FAIL: {path} must contain a JSON object; not touching it.", file=sys.stderr)
        sys.exit(1)
    hooks = data.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        print(f"FAIL: {path} has a non-object 'hooks' key; not touching it.", file=sys.stderr)
        sys.exit(1)
    for event, entries in entries_for(index).items():
        groups = [group for group in hooks.get(event, []) if not is_ours(group)]
        if mode == "install":
            groups.extend(entries)
        if groups:
            hooks[event] = groups
        else:
            hooks.pop(event, None)
    rendered = json.dumps(data, indent=2) + "\n"
    if dry_run:
        print(f"--- {path} ({mode}, dry run)")
        print(rendered, end="")
        continue
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_file():
        backup = path.with_name(path.name + ".bak-jev-" + time.strftime("%Y%m%d%H%M%S"))
        backup.write_bytes(path.read_bytes())
        print(f"backup: {backup}")
    path.write_text(rendered, encoding="utf-8")
    print(f"{mode}ed Jev and Workflow hooks in {path}")
PY

if [ "$DRY_RUN" = "1" ]; then
  printf -- '--- %s (%s, dry run)\n%s\n' "$ROOT_FILE" "$MODE" "$HARNESS_ROOT"
elif [ "$MODE" = "install" ]; then
  mkdir -p "$(dirname "$ROOT_FILE")"
  printf '%s\n' "$HARNESS_ROOT" > "$ROOT_FILE"
  printf 'installed harness root %s in %s\n' "$HARNESS_ROOT" "$ROOT_FILE"
elif [ -f "$ROOT_FILE" ] && [ "$(head -n 1 "$ROOT_FILE")" = "$HARNESS_ROOT" ]; then
  rm -f "$ROOT_FILE"
  printf 'removed harness root record %s\n' "$ROOT_FILE"
fi
