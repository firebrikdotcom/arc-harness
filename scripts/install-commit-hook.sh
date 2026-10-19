#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
REPO_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
EXPECTED="$REPO_ROOT/.githooks"

if ! git -C "$REPO_ROOT" rev-parse --show-toplevel >/dev/null 2>&1; then
  printf 'Refusing to install: %s is not inside a Git worktree.\n' "$REPO_ROOT" >&2
  exit 2
fi
if [ ! -x "$EXPECTED/pre-commit" ]; then
  printf 'Refusing to install: pre-commit hook is missing or not executable: %s/pre-commit\n' "$EXPECTED" >&2
  exit 2
fi

CURRENT=$(git -C "$REPO_ROOT" config --local --get core.hooksPath || true)
if [ -n "$CURRENT" ] && [ "$CURRENT" != "$EXPECTED" ]; then
  printf 'Refusing to replace existing core.hooksPath: %s\n' "$CURRENT" >&2
  printf 'Review it and compose the hooks manually if both are needed.\n' >&2
  exit 1
fi

git -C "$REPO_ROOT" config --local core.hooksPath "$EXPECTED"
printf 'Installed local commit check: %s/pre-commit\n' "$EXPECTED"
printf '%s\n' 'This setting is local Git configuration and does not change CI.'
