#!/usr/bin/env sh
# Orbs start only through the gated dispatcher: installs deny Claude Code starting
# one directly, drop an earlier allow rule, and the guard refuses direct launches.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INSTALL="$HARNESS_ROOT_UNDER_TEST/scripts/install-hooks.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-amp-orb.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
CLAUDE="$TMP_ROOT/claude/settings.json"
CODEX="$TMP_ROOT/codex/hooks.json"

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

install() {
  "$INSTALL" --claude "$CLAUDE" --codex "$CODEX" --root-file "$TMP_ROOT/root" "$@" >/dev/null
}

rules() {
  python3 -c 'import json, sys; p = json.load(open(sys.argv[1])).get("permissions", {}); print(json.dumps([p.get("allow"), p.get("deny")]))' "$CLAUDE"
}

mkdir -p "$(dirname "$CLAUDE")"
printf '%s\n' '{"permissions": {"allow": ["Bash(ls:*)", "Bash(amp -ox:*)"], "deny": ["Bash(rm:*)"]}}' > "$CLAUDE"

install
install
expected='[["Bash(ls:*)"], ["Bash(rm:*)", "Bash(amp -ox:*)", "Bash(amp --orb-execute:*)"]]'
[ "$(rules)" = "$expected" ] || fail "install should drop the allow rule and deny direct orbs once, got $(rules)"
grep -q 'amp -ox' "$CODEX" && fail "Codex hooks must not receive Claude permission rules"

# A dry run reports without writing.
"$INSTALL" --claude "$CLAUDE" --codex "$CODEX" --root-file "$TMP_ROOT/root" --uninstall --dry-run >/dev/null
[ "$(rules)" = "$expected" ] || fail "dry run changed the settings"

install --uninstall
[ "$(rules)" = '[["Bash(ls:*)"], ["Bash(rm:*)"]]' ] || fail "uninstall should remove only the orb rules, got $(rules)"

# The guard refuses direct orb launches and direct prompt-hook calls, however they are spelled.
python3 - "$HARNESS_ROOT_UNDER_TEST" <<'PY'
import sys
from pathlib import Path
root = Path(sys.argv[1])
sys.path.insert(0, str(root / "scripts"))
import permit
def denied(command):
    return permit.check_command(command, root, str(root)) is not None
for command in ("amp -ox 'do it'", "cd /x && /usr/local/bin/amp --title t -ox", "amp --orb-execute -x hi",
                "amp -x hi --executor orb", "amp -x hi --executor=orb", "echo '{}' | python3 scripts/workflow_gate.py claude-code",
                "scripts/workflow_gate.py claude-code", "env amp -ox hi", "nohup amp -ox hi", "timeout 900 amp -ox hi",
                "bash -c 'amp -ox hi'", "sh -c \"cd /tmp; amp -ox hi\"", "echo hi | xargs amp -ox",
                "env python3 scripts/workflow_gate.py", "timeout 5 /usr/bin/python3 -I scripts/workflow_gate.py",
                "bash -c 'python3 scripts/workflow_gate.py'", "script -q /dev/null amp -ox hi", "eval \"amp -ox hi\"",
                "HARNESS_WORKFLOW_STATE=/tmp/x.sqlite python3 scripts/role_dispatch.py create",
                "export HARNESS_SESSION_ID=other", "CLAUDE_SESSION_ID=other python3 x.py"):
    assert denied(command), command
for command in ("amp threads export T-1", "amp -x 'local run'", "grep -n orb scripts/workflow_gate.py",
                "sed -n 1,20p scripts/workflow_gate.py", "python3 tests/test_workflow_audit.py", "amp projects status",
                "HARNESS_SESSION_ID= sh tests/x.sh", "git commit -m 'Deny amp -ox; start orbs via the dispatcher'"):
    assert not denied(command), command
PY

printf '%s\n' "PASS: orbs start only through the gated dispatcher"
