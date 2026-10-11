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
  JG="scripts/jg.sh"
  DOCS="docs"
  TASKS="tasks"
  VERIFY="scripts/verify.sh"
  REVIEW="scripts/review.sh"
else
  CLI="$HARNESS_ROOT/scripts/harness"
  JG="$HARNESS_ROOT/scripts/jg.sh"
  DOCS="$HARNESS_ROOT/docs"
  TASKS="$HARNESS_ROOT/tasks"
  VERIFY="$HARNESS_ROOT/scripts/verify.sh"
  REVIEW="$HARNESS_ROOT/scripts/review.sh"
fi

block_file="$PROJECT_ROOT/.harness-guide-block.tmp.$$"
trap 'rm -f "$block_file"' EXIT HUP INT TERM
# Keep this block short: it is read at every session start. Details live in
# the harness docs it names, read only when a task needs them.
cat > "$block_file" <<BLOCK
$START_MARK
## Harness

Work runs in phases, and a guard enforces each rule below. Edits are allowed only in build.

\`\`\`sh
$CLI plan start                  # read; write the task contract ($TASKS/task.example.json)
$CLI contract set task.json      # or: $CLI contract waive "<why there is nothing to accept>"
$CLI plan done                   # needs the contract
$CLI build start                 # make the change
$VERIFY --project .              # evidence: fails when no check ran
$CLI build done                  # needs that verify on the current files and every acceptance command
$CLI review start
$REVIEW --project .              # writes a packet for an independent reviewer (fresh context)
$CLI review submit findings.json # a person submits a fresh-context reviewer's verdict
$CLI review done                 # needs an approval of the current files; no non-goal path touched
\`\`\`

- Session start prints \`$CLI brief\`: this session's run (or the latest), its contract, any pause, recent steps, and the project map. Record steps with \`$CLI step --note "..."\`.
- A pause (budget, or the same failure twice) waits for the user. Resume only on their instruction: \`$CLI continue "<new approach>"\`. Abort is theirs.
- Jev (TypeSafe) advice and the task launcher: $DOCS/jev-checkpoints.md, $DOCS/setup.md. \`$CLI jev status\` says whether Jev is followed: off, every call is a shadow comparison and your own baseline runs; on, the session-start route is the route to take and \`$CLI advise\` returns Jev's choice as \`action\` with \`delegated: true\`, so take that action unless a deterministic rule (permissions, required checks, failures, the user's choice) decides otherwise, then label the call. Semantic search: \`$JG --project PATH "question"\`.
$END_MARK
BLOCK

install_block() {
  target="$1"
  tmp="$target.tmp.$$"
  # A CLAUDE.md that imports AGENTS.md already has the block; one copy is enough.
  if [ "$(basename "$target")" = "CLAUDE.md" ] && [ -f "$target" ] && grep -qx '@AGENTS.md' "$target"; then
    info "skipped: $target (imports AGENTS.md)"
    return 0
  fi
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
