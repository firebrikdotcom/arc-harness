#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CLEANUP="$HARNESS_ROOT_UNDER_TEST/scripts/worktree_cleanup.py"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-worktree-cleanup.XXXXXX")
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

REPO="$TMP_ROOT/repo"
g() { git -C "$REPO" -c user.name=t -c user.email=t@example.com "$@"; }
git init -q -b main "$REPO"
g commit -q --allow-empty -m base

# merged: own commit, merged into main, clean.
g worktree add -q -b task/merged "$TMP_ROOT/merged"
git -C "$TMP_ROOT/merged" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m work
g merge -q --no-edit task/merged
# unmerged: own commit on no other branch.
g worktree add -q -b task/unmerged "$TMP_ROOT/unmerged"
git -C "$TMP_ROOT/unmerged" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m mine
# fresh: a task worktree not started yet.
g worktree add -q -b task/fresh "$TMP_ROOT/fresh"
# dirty: merged, but holds an untracked file.
g worktree add -q -b task/dirty "$TMP_ROOT/dirty"
git -C "$TMP_ROOT/dirty" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m d
g merge -q --no-edit task/dirty
printf 'x\n' > "$TMP_ROOT/dirty/notes.txt"
# locked: merged, but locked.
g worktree add -q -b task/locked "$TMP_ROOT/locked"
git -C "$TMP_ROOT/locked" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m l
g merge -q --no-edit task/locked
g worktree lock "$TMP_ROOT/locked"

# Recent use keeps every worktree by default.
out=$(python3 "$CLEANUP" --project "$REPO" --dry-run)
printf '%s\n' "$out" | grep -q "kept: $TMP_ROOT/merged (used in the last 60 minutes)" || fail "recent worktree should be kept: $out"

out=$(python3 "$CLEANUP" --project "$REPO" --min-idle 0)
printf '%s\n' "$out" | grep -q "removed: $TMP_ROOT/merged" || fail "merged worktree should go: $out"
printf '%s\n' "$out" | grep -q "deleted branch: task/merged" || fail "merged branch should be deleted: $out"
[ ! -d "$TMP_ROOT/merged" ] || fail "merged worktree directory remains"
printf '%s\n' "$out" | grep -q "kept: $TMP_ROOT/unmerged (commits not on any other branch)" || fail "unmerged kept: $out"
printf '%s\n' "$out" | grep -q "kept: $TMP_ROOT/fresh (no commits of its own yet)" || fail "fresh kept: $out"
printf '%s\n' "$out" | grep -q "kept: $TMP_ROOT/dirty (uncommitted or untracked files)" || fail "dirty kept: $out"
printf '%s\n' "$out" | grep -q "kept: $TMP_ROOT/locked (locked)" || fail "locked kept: $out"
[ -f "$TMP_ROOT/dirty/notes.txt" ] || fail "dirty worktree lost its file"
g rev-parse -q --verify task/unmerged >/dev/null || fail "unmerged branch must stay"

# A process working inside a worktree keeps it.
g worktree add -q -b task/busy "$TMP_ROOT/busy"
git -C "$TMP_ROOT/busy" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m b
g merge -q --no-edit task/busy
(cd "$TMP_ROOT/busy" && sleep 30) &
busy_pid=$!
sleep 1
out=$(python3 "$CLEANUP" --project "$REPO" --min-idle 0)
kill "$busy_pid" 2>/dev/null || true
printf '%s\n' "$out" | grep -q "kept: $TMP_ROOT/busy (a process is using it)" || fail "busy worktree kept: $out"

# Hook mode ignores other commands and never fails the call.
printf '%s' '{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"cwd":"/nowhere"}' |
  python3 "$CLEANUP" --hook || fail "hook should exit 0"
printf '%s' 'not json' | python3 "$CLEANUP" --hook || fail "hook should exit 0 on bad input"

printf '%s\n' 'worktree-cleanup: ok'
