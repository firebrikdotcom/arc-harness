#!/usr/bin/env sh
# Install the jevgrep guidance that interactive Claude Code and Codex sessions
# see, so that searches go through scripts/jg.sh rather than a bare `jg`.
#
#   scripts/install-jg-skill.sh [--home DIR] [--source SKILL.md | --no-upstream] [--skip-global]
#
# It writes two things, both idempotently:
#   1. The skill: `<home>/.claude/skills/jevgrep/SKILL.md` and
#      `<home>/.agents/skills/jevgrep/SKILL.md` are refreshed from the upstream
#      package skill (an explicit --source, or the one next to the installed
#      `jg`) and end with a marked harness block that redirects searches to the
#      wrapper. Without an upstream source an existing file keeps its upstream
#      part and only the block is refreshed; a missing file is then skipped.
#      --no-upstream skips the package lookup and forces that block-only mode.
#   2. The global guides: when `<home>/.claude/CLAUDE.md` or
#      `<home>/.codex/AGENTS.md` carries the `global-harness` block, one marked
#      retrieval paragraph is inserted (or refreshed) before its end marker.
#      Project-owned guides are never touched; use install-guides.sh for those.
# Exit 2 for usage errors or when no skill could be written at all.
set -u

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HOME_DIR=${HOME:-}
SOURCE=""
SKIP_GLOBAL=0
NO_UPSTREAM=0
SKILL_START='<!-- harness-jg:start -->'
SKILL_END='<!-- harness-jg:end -->'
GLOBAL_END='<!-- global-harness:end -->'

info() {
  printf '%s\n' "$*"
}

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --home)
      [ $# -ge 2 ] || { info "FAIL: --home requires a directory"; exit 2; }
      HOME_DIR=$2
      shift 2
      ;;
    --source)
      [ $# -ge 2 ] || { info "FAIL: --source requires a file"; exit 2; }
      SOURCE=$2
      shift 2
      ;;
    --skip-global)
      SKIP_GLOBAL=1
      shift
      ;;
    --no-upstream)
      NO_UPSTREAM=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      info "FAIL: unknown argument: $1"
      usage
      exit 2
      ;;
  esac
done

if [ -z "$HOME_DIR" ] || [ ! -d "$HOME_DIR" ]; then
  info "FAIL: home directory does not exist: ${HOME_DIR:-<empty>}"
  exit 2
fi
HOME_DIR=$(CDPATH='' cd "$HOME_DIR" && pwd -P)

# Locate the upstream skill: explicit source, then the package that owns the
# installed `jg` binary, then the usual global package roots (npm, Homebrew,
# nvm under the selected home).
skill_in_package() {
  for candidate in "$1/dist/skills/jevgrep/SKILL.md" "$1/skills/jevgrep/SKILL.md"; do
    if [ -f "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

find_source() {
  if [ "$NO_UPSTREAM" -eq 1 ]; then
    return 1
  fi
  if [ -n "$SOURCE" ]; then
    printf '%s\n' "$SOURCE"
    return 0
  fi
  jg_bin=$(command -v jg 2>/dev/null || :)
  if [ -n "$jg_bin" ]; then
    resolved=$(readlink -f "$jg_bin" 2>/dev/null || printf '%s' "$jg_bin")
    case "$resolved" in
      */@dzhng/jevgrep/*)
        skill_in_package "${resolved%%/@dzhng/jevgrep/*}/@dzhng/jevgrep" && return 0
        ;;
    esac
  fi
  for root in \
    "$(npm root -g 2>/dev/null || :)" \
    "$(brew --prefix 2>/dev/null || :)/lib/node_modules" \
    "${NVM_DIR:-$HOME_DIR/.nvm}"/versions/node/*/lib/node_modules \
    "$HOME_DIR/.config/nvm"/versions/node/*/lib/node_modules; do
    [ -d "$root" ] || continue
    skill_in_package "$root/@dzhng/jevgrep" && return 0
  done
  return 1
}

if [ -n "$SOURCE" ] && [ ! -f "$SOURCE" ]; then
  info "FAIL: --source file does not exist: $SOURCE"
  exit 2
fi
UPSTREAM=$(find_source) || UPSTREAM=""

WRAPPER="$HARNESS_ROOT/scripts/jg.sh"
block_file=$(mktemp "${TMPDIR:-/tmp}/harness-jg-block.XXXXXX")
trap 'rm -f "$block_file"' EXIT HUP INT TERM
cat > "$block_file" <<BLOCK
$SKILL_START
## Harness targets

Inside a development-harness target (any repository registered by the harness, which includes every project a session auto-initialises), do not run \`jg\` directly. Run the harness wrapper instead:

\`\`\`sh
$WRAPPER --project /absolute/path/to/project "question"
$WRAPPER --project /absolute/path/to/project --root src/subdir "question"
$WRAPPER --project /absolute/path/to/project --report
\`\`\`

The wrapper searches the same tree and prints the same output, but it also writes a compact private retrieval record (question hash, timing, exit, completeness; never the question, paths, or excerpts) so the harness can see that retrieval was used, and it refuses \`--include-sensitive\`, \`--no-ignore\`, and any target carrying a \`.harness-no-upload\` marker, because \`jg\` uploads eligible source to the provider saved by \`jg auth\`. Use one retrieval before broad grepping on an unfamiliar question, then read the cited files. Never enter the provider key in chat; \`jg auth\` is run by the user.
$SKILL_END
BLOCK

strip_block() {
  # Print the file without an existing harness block (and one blank line before it).
  awk -v start="$SKILL_START" -v end="$SKILL_END" '
    $0 == start { skipping = 1; next }
    $0 == end { skipping = 0; next }
    !skipping { print }
  ' "$1" | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}'
}

written=0
install_skill() {
  target="$1"
  dir=$(dirname "$target")
  tmp="$dir/SKILL.md.tmp.$$"
  if [ -n "$UPSTREAM" ]; then
    mkdir -p "$dir"
    {
      cat "$UPSTREAM"
      printf '\n'
      cat "$block_file"
    } > "$tmp"
    action="refreshed from upstream"
  elif [ -f "$target" ]; then
    {
      strip_block "$target"
      printf '\n'
      cat "$block_file"
    } > "$tmp"
    action="block refreshed (no upstream source found)"
  else
    info "skipped: $target (no upstream skill found; install @dzhng/jevgrep or pass --source)"
    return 0
  fi
  if [ -f "$target" ] && cmp -s "$tmp" "$target"; then
    rm -f "$tmp"
    info "unchanged: $target"
  else
    mv "$tmp" "$target"
    info "$action: $target"
  fi
  written=$((written + 1))
}

install_global() {
  target="$1"
  [ -f "$target" ] || { info "skipped: $target (missing)"; return 0; }
  grep -qF "$GLOBAL_END" "$target" || { info "skipped: $target (no global-harness block)"; return 0; }
  tmp="$target.tmp.$$"
  awk -v start="$SKILL_START" -v end="$SKILL_END" -v gend="$GLOBAL_END" -v wrapper="$WRAPPER" '
    $0 == start { skipping = 1; next }
    $0 == end { skipping = 0; next }
    skipping { next }
    $0 == gend {
      print start
      print "Before broad grepping on an unfamiliar question, run one semantic retrieval through the harness wrapper: `" wrapper " --project /absolute/path/to/project \"question\"` (add `--root SUBDIR` to narrow it). It runs jevgrep (`jg`), records only a question hash, timing, and exit under the target database, and refuses upload-widening flags and any target carrying a `.harness-no-upload` marker. Do not call `jg` directly on a target, and never enter its key in chat."
      print end
    }
    { print }
  ' "$target" > "$tmp"
  if cmp -s "$tmp" "$target"; then
    rm -f "$tmp"
    info "unchanged: $target"
  else
    mv "$tmp" "$target"
    info "refreshed: $target"
  fi
}

info "Installing jevgrep guidance"
info "Harness root: $HARNESS_ROOT"
info "Home:         $HOME_DIR"
info "Upstream:     ${UPSTREAM:-<none found>}"
install_skill "$HOME_DIR/.claude/skills/jevgrep/SKILL.md"
install_skill "$HOME_DIR/.agents/skills/jevgrep/SKILL.md"
if [ "$SKIP_GLOBAL" -eq 0 ]; then
  install_global "$HOME_DIR/.claude/CLAUDE.md"
  install_global "$HOME_DIR/.codex/AGENTS.md"
fi

if [ "$written" -eq 0 ]; then
  info "FAIL: no skill file was written"
  exit 2
fi
exit 0
