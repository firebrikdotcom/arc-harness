#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HARNESS_DB_ROOT="${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}"
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/review.sh [--project PATH]"
  info ""
  info "Runs project verification and prints target/harness diff summaries."
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

PROJECT_ROOT=$(cd "$PROJECT_ROOT" && pwd -P)

info "Review started"
info "Harness root: $HARNESS_ROOT"
info "Project root: $PROJECT_ROOT"
info ""

if [ ! -x "$SCRIPT_DIR/verify.sh" ]; then
  info "FAIL: scripts/verify.sh is missing or not executable."
  exit 1
fi

"$SCRIPT_DIR/verify.sh" --project "$PROJECT_ROOT"

info ""
info "==> target git diff summary"
if command -v git >/dev/null 2>&1 && git -C "$PROJECT_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git -C "$PROJECT_ROOT" status --short -- .
  info ""
  git -C "$PROJECT_ROOT" diff --stat -- .
else
  info "SKIP: git is unavailable or project root is not a git worktree."
fi

if [ "$PROJECT_ROOT" != "$HARNESS_ROOT" ]; then
  info ""
  info "==> harness git diff summary"
  if command -v git >/dev/null 2>&1 && git -C "$HARNESS_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$HARNESS_ROOT" status --short -- .
    info ""
    git -C "$HARNESS_ROOT" diff --stat -- .
  else
    info "SKIP: git is unavailable or harness root is not a git worktree."
  fi
fi

info ""
info "Review questions:"
info "1. Does it satisfy acceptance criteria?"
info "2. Are tests meaningful?"
info "3. Did we avoid scope creep?"
info "4. Are docs/progress updated?"
info "5. Are there security or performance risks?"

# Write the KEY=VALUE record that `scripts/harness review done` requires.
records_dir="$HARNESS_DB_ROOT/records"
if mkdir -p "$records_dir" 2>/dev/null; then
  git_head=$(git -C "$PROJECT_ROOT" rev-parse HEAD 2>/dev/null || printf 'unknown')
  record="$records_dir/review.state"
  {
    printf 'RECORD_KIND=review\n'
    printf 'RECORD_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'RECORD_EPOCH=%s\n' "$(date +%s)"
    printf 'PROJECT_ROOT=%s\n' "$PROJECT_ROOT"
    printf 'GIT_HEAD=%s\n' "$git_head"
    printf 'EXIT=0\n'
  } > "$record.tmp.$$"
  mv "$record.tmp.$$" "$record"
  info ""
  info "Run record: $record"
else
  info "WARN: could not create $records_dir; no review record written."
fi
