#!/usr/bin/env sh
set -eu

# Tracked files describe a generic harness: no personal home directories,
# machine hostnames, or checkout locations. Local locations belong in the
# environment or the gitignored .env (see .env.example). Examples may use the
# placeholder users me, user, runner, and example.
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
cd "$ROOT"

# Home directories, tailnet and mDNS hostnames, and code checkouts under a home.
PATTERN='(^|[^A-Za-z0-9_.}-])(/home|/Users)/[A-Za-z0-9._-]+/'
PATTERN="$PATTERN"'|[A-Za-z0-9-]+\.ts\.net\b'
PATTERN="$PATTERN"'|(^|[^A-Za-z0-9._/-])[A-Za-z0-9-]+\.local([^A-Za-z0-9./_-]|$)'
# shellcheck disable=SC2016 # $HOME is matched literally in tracked text
PATTERN="$PATTERN"'|(~|\$HOME|\$\{HOME\}|%h|__HOME__)/Code/'

found=$(git grep -nIE "$PATTERN" -- . ':!tests/personal-paths.sh' \
  | grep -vE '(/home|/Users)/(me|user|runner|example)/' || true)

# The running user's own home is personal too, unless it is a placeholder user.
case "${HOME:-}" in
  /home/me|/home/user|/home/runner|/home/example|/Users/me|/Users/user|/Users/runner|/Users/example|/root|/|'') ;;
  *)
    own=$(git grep -nIF "$HOME/" -- . ':!tests/personal-paths.sh' || true)
    found=$(printf '%s\n%s\n' "$found" "$own")
    ;;
esac

# So is this machine's own name. LICENSE is skipped: its copyright holder's
# name may also name a machine.
host=$(hostname -s 2>/dev/null || hostname 2>/dev/null || true)
case "$host" in
  ''|localhost|localhost.*) ;;
  *)
    if [ "${#host}" -ge 4 ]; then
      own=$(git grep -nIwiF "$host" -- . ':!tests/personal-paths.sh' ':!LICENSE' || true)
      found=$(printf '%s\n%s\n' "$found" "$own")
    fi
    ;;
esac

found=$(printf '%s\n' "$found" | sed '/^$/d' | sort -u)
if [ -n "$found" ]; then
  printf '%s\n' 'FAIL: tracked files name a personal path or host; use a placeholder or a .env setting:' >&2
  printf '%s\n' "$found" >&2
  exit 1
fi
printf '%s\n' 'PASS: no personal home paths, hostnames, or checkout locations in tracked files'
