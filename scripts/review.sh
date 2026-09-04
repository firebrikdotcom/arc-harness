#!/usr/bin/env sh
set -u

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

# Verification failing is exactly when a reviewer needs the evidence below,
# so its status is captured and the review continues.
verify_status=0
"$SCRIPT_DIR/verify.sh" --project "$PROJECT_ROOT" || verify_status=$?
if [ "$verify_status" -ne 0 ]; then
  info ""
  info "WARN: verification failed (exit $verify_status); continuing the review so the change can still be inspected."
fi

# show_patch ROOT LABEL  Prints status, the full staged and unstaged patch, and
# every untracked file as a new-file diff.
show_patch() {
  root="$1"
  label="$2"
  info ""
  info "==> $label changes"
  if ! command -v git >/dev/null 2>&1 || ! git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    info "SKIP: git is unavailable or $label root is not a git worktree."
    return 0
  fi
  git -C "$root" status --short -- .
  info ""
  info "--- $label patch: staged"
  git -C "$root" --no-pager diff --cached -- .
  info ""
  info "--- $label patch: unstaged"
  git -C "$root" --no-pager diff -- .
  info ""
  info "--- $label patch: untracked files"
  git -C "$root" ls-files --others --exclude-standard -- . | while IFS= read -r untracked; do
    [ -n "$untracked" ] || continue
    git -C "$root" --no-pager diff --no-index -- /dev/null "$untracked" || true
  done
}

show_patch "$PROJECT_ROOT" "target"
if [ "$PROJECT_ROOT" != "$HARNESS_ROOT" ]; then
  show_patch "$HARNESS_ROOT" "harness"
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
    printf 'VERIFY_EXIT=%s\n' "$verify_status"
    printf 'EXIT=%s\n' "$verify_status"
  } > "$record.tmp.$$"
  mv "$record.tmp.$$" "$record"
  info ""
  info "Run record: $record"
else
  info "WARN: could not create $records_dir; no review record written."
fi

if [ "$verify_status" -ne 0 ]; then
  info "Review finished with verification failures (exit $verify_status)."
  exit "$verify_status"
fi
info "Review finished."
