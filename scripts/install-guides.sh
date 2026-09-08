#!/usr/bin/env sh
set -eu

# Adds or refreshes a marked "Harness Phases" block in AGENTS.md and CLAUDE.md
# of a project so agents learn the harness commands from the files they read
# first. Idempotent: an existing block is replaced in place, a missing file is
# created, and any other content is left untouched.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"
START_MARK='<!-- harness-cli:start -->'
END_MARK='<!-- harness-cli:end -->'

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/install-guides.sh [--project PATH]"
  info ""
  info "Adds or refreshes the harness command block in AGENTS.md and CLAUDE.md."
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --project)
      if [ "$#" -lt 2 ]; then
        info "FAIL: --project requires a path."
        exit 2
      fi
      PROJECT_ROOT="$2"
      shift 2
      ;;
    --help|-h)
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

if [ ! -d "$PROJECT_ROOT" ]; then
  info "FAIL: project root does not exist or is not a directory: $PROJECT_ROOT"
  exit 2
fi
PROJECT_ROOT=$(CDPATH='' cd "$PROJECT_ROOT" && pwd -P)

# Inside the harness root a relative command is convenient. External projects
# use the absolute CLI path, which now resolves its root from its own location.
if [ "$PROJECT_ROOT" = "$HARNESS_ROOT" ]; then
  CLI="scripts/harness"
else
  CLI="$HARNESS_ROOT/scripts/harness"
fi

block_file="$PROJECT_ROOT/.harness-guide-block.tmp.$$"
trap 'rm -f "$block_file"' EXIT HUP INT TERM
cat > "$block_file" <<BLOCK
$START_MARK
## Harness Phases

Every session runs inside a harness phase. Open one before editing files or running commands. Where the phase guard hook is installed, Write, Edit, and Bash are blocked until a phase is active.

\`\`\`sh
$CLI plan start      # read, scope the task, record the plan in progress.md
$CLI plan done
$CLI build start     # implement; run scripts/verify.sh before finishing
$CLI build done
$CLI review start    # run scripts/review.sh and inspect the diff
$CLI review done
\`\`\`

Record work with \`$CLI step --note "..."\`. When blocked, run \`$CLI status\`. After a budget pause, evaluate and run \`$CLI continue "<evaluation note>"\`.
$END_MARK
BLOCK

install_block() {
  target="$1"
  tmp="$target.tmp.$$"
  if [ ! -f "$target" ]; then
    {
      printf '# %s\n\n' "$(basename "$target" .md)"
      cat "$block_file"
    } > "$tmp"
    mv "$tmp" "$target"
    info "created: $target"
    return 0
  fi

  if grep -qF "$START_MARK" "$target" && grep -qF "$END_MARK" "$target"; then
    awk -v start="$START_MARK" -v end="$END_MARK" -v block="$block_file" '
      $0 == start { while ((getline line < block) > 0) print line; skipping = 1; next }
      $0 == end { skipping = 0; next }
      !skipping { print }
    ' "$target" > "$tmp"
    mv "$tmp" "$target"
    info "refreshed: $target"
    return 0
  fi

  {
    cat "$target"
    # Ensure exactly one blank line before the block.
    if [ -s "$target" ] && [ "$(tail -c 1 "$target" | od -An -c | tr -d ' ')" != '\n' ]; then
      printf '\n'
    fi
    printf '\n'
    cat "$block_file"
  } > "$tmp"
  mv "$tmp" "$target"
  info "appended: $target"
}

info "Installing harness guide block"
info "Harness root: $HARNESS_ROOT"
info "Project root: $PROJECT_ROOT"
install_block "$PROJECT_ROOT/AGENTS.md"
install_block "$PROJECT_ROOT/CLAUDE.md"
