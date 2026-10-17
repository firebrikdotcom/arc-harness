#!/usr/bin/env sh
# Install the firebrik-sites skill for Claude Code and Codex, pointing at this
# checkout's `harness sites` command.
#
#   scripts/install-sites-skill.sh [--home DIR]
#
# Renders skills/firebrik-sites/SKILL.md with this checkout's absolute
# scripts/harness path into `<home>/.claude/skills/firebrik-sites/SKILL.md`
# and `<home>/.agents/skills/firebrik-sites/SKILL.md`, idempotently. A
# symlinked skill directory is followed, so a shared ~/.agents copy linked
# from ~/.claude is written once. Exit 2 for usage errors.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HOME_DIR=${HOME:-}
SOURCE="$HARNESS_ROOT/skills/firebrik-sites/SKILL.md"

info() {
  printf '%s\n' "$*"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --home)
      [ $# -ge 2 ] || { info "FAIL: --home requires a directory"; exit 2; }
      HOME_DIR=$2
      shift 2
      ;;
    -h|--help)
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      info "FAIL: unknown argument: $1"
      exit 2
      ;;
  esac
done

if [ -z "$HOME_DIR" ] || [ ! -d "$HOME_DIR" ]; then
  info "FAIL: home directory does not exist: ${HOME_DIR:-<empty>}"
  exit 2
fi
[ -f "$SOURCE" ] || { info "FAIL: missing skill source: $SOURCE"; exit 2; }

rendered=$(mktemp "${TMPDIR:-/tmp}/harness-sites-skill.XXXXXX")
trap 'rm -f "$rendered"' EXIT HUP INT TERM
# The harness path is absolute and has no '|', so it is a safe sed replacement.
sed "s|@HARNESS@|$HARNESS_ROOT/scripts/harness|g" "$SOURCE" > "$rendered"

for target in "$HOME_DIR/.claude/skills/firebrik-sites/SKILL.md" "$HOME_DIR/.agents/skills/firebrik-sites/SKILL.md"; do
  mkdir -p "$(dirname "$target")"
  if [ -f "$target" ] && cmp -s "$rendered" "$target"; then
    info "unchanged: $target"
  else
    cp "$rendered" "$target.tmp.$$"
    mv "$target.tmp.$$" "$target"
    info "installed: $target"
  fi
done
