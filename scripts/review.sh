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
  info "Runs project verification, prints the target/harness diff, and writes a review"
  info "packet (task contract, verification output, diff) for an independent reviewer."
  info "With HARNESS_REVIEWER_CMD set, it runs that reviewer itself:"
  info "  sh -c \"\$HARNESS_REVIEWER_CMD\" reviewer PACKET FINDINGS_OUT"
  info "The diff includes commits since HARNESS_REVIEW_BASE, else since the run's plan."
  info "Otherwise hand the packet to a reviewer in a fresh context and submit its JSON"
  info "with: harness review submit FILE. 'harness review done' requires an approving"
  info "verdict for the current files (schemas/review-findings.schema.json)."
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

# A registered target (scripts/harness-target.sh) owns its own database
# beneath the database root.
if [ -x "$SCRIPT_DIR/harness-target.sh" ]; then
  target_db=$(HARNESS_DB_ROOT=$HARNESS_DB_ROOT "$SCRIPT_DIR/harness-target.sh" db-root "$PROJECT_ROOT" 2>/dev/null || :)
  if [ -n "$target_db" ]; then
    HARNESS_DB_ROOT=$target_db
  fi
fi

info "Review started"
info "Harness root: $HARNESS_ROOT"
info "Project root: $PROJECT_ROOT"
info ""

if [ ! -x "$SCRIPT_DIR/verify.sh" ]; then
  info "FAIL: scripts/verify.sh is missing or not executable."
  exit 1
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/harness-review.XXXXXX")
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

# Verification failing is exactly when a reviewer needs the evidence below,
# so its status is captured and the review continues.
{ "$SCRIPT_DIR/verify.sh" --project "$PROJECT_ROOT"; printf '%s\n' "$?" > "$WORK/verify.status"; } 2>&1 | tee "$WORK/verify.log"
verify_status=$(cat "$WORK/verify.status" 2>/dev/null || printf '1')
if [ "$verify_status" -ne 0 ]; then
  info ""
  info "WARN: verification failed (exit $verify_status); continuing the review so the change can still be inspected."
fi

# Shadow Jev handoff checkpoint; observes only, enabled by HARNESS_JEV_CHECKPOINTS=1.
if [ "${HARNESS_JEV_CHECKPOINTS:-0}" = "1" ] && command -v python3 >/dev/null 2>&1 && [ -f "$SCRIPT_DIR/phase_checkpoint.py" ]; then
  python3 "$SCRIPT_DIR/phase_checkpoint.py" review-handoff --verify-exit "$verify_status" --project "$PROJECT_ROOT" --db-root "$HARNESS_DB_ROOT" 2>/dev/null | while IFS= read -r jev_line; do
    info "Jev: $jev_line"
  done
fi

records_dir=$(sh "$SCRIPT_DIR/run-paths.sh" records "$HARNESS_DB_ROOT")
current_file=$(sh "$SCRIPT_DIR/run-paths.sh" current "$HARNESS_DB_ROOT")
run_id=$(head -n 1 "$current_file" 2>/dev/null || :)
# Committed work belongs in the review too: from HARNESS_REVIEW_BASE, else the
# commit the run's plan was closed on (CONTRACT_BASE).
review_base=${HARNESS_REVIEW_BASE:-}
if [ -z "$review_base" ] && [ -n "$run_id" ] && [ -f "$HARNESS_DB_ROOT/runs/$run_id/state" ]; then
  review_base=$(sed -n 's/^CONTRACT_BASE=//p' "$HARNESS_DB_ROOT/runs/$run_id/state" | tail -n 1)
fi

# show_patch ROOT LABEL [BASE]  Prints status, the commits and committed patch
# since BASE, the full staged and unstaged patch, and every untracked file as a
# new-file diff.
show_patch() {
  root="$1"
  label="$2"
  base="${3:-}"
  info ""
  info "==> $label changes"
  if ! command -v git >/dev/null 2>&1 || ! git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    info "SKIP: git is unavailable or $label root is not a git worktree."
    return 0
  fi
  if [ -n "$base" ] && git -C "$root" rev-parse --verify -q "$base^{commit}" >/dev/null; then
    info "--- $label commits since $base"
    git -C "$root" --no-pager log --oneline "$base..HEAD" -- .
    info ""
    info "--- $label patch: committed since $base"
    git -C "$root" --no-pager diff "$base" HEAD -- .
    info ""
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

{
  show_patch "$PROJECT_ROOT" "target" "$review_base"
  if [ "$PROJECT_ROOT" != "$HARNESS_ROOT" ]; then
    show_patch "$HARNESS_ROOT" "harness"
  fi
} | tee "$WORK/patch.txt"

tree_hash=$(sh "$SCRIPT_DIR/tree-hash.sh" "$PROJECT_ROOT")
contract="$HARNESS_DB_ROOT/runs/$run_id/task.json"
if ! mkdir -p "$records_dir" 2>/dev/null; then
  info "WARN: could not create $records_dir; no review record written."
  exit "$verify_status"
fi

# The packet is everything an independent reviewer needs and nothing else: it
# holds no conversation, so the reviewer judges the change, not the narrative.
packet="$records_dir/review-packet.md"
{
  printf '%s\n' "# Review packet" ""
  printf '%s\n' "You are an independent reviewer. You did not write this change. Check it against the"
  printf '%s\n' "task contract and the evidence below. Report every defect you can point to with a"
  printf '%s\n' "file and line or reproduce with a command; do not report style preferences."
  printf '%s\n' "Return JSON matching schemas/review-findings.schema.json (template below) with"
  printf '%s\n' "verdict \"block\" if any blocker or major finding is open, otherwise \"approve\"." ""
  printf 'Project: %s\nTree hash: %s\n\n' "$PROJECT_ROOT" "$tree_hash"
  printf '%s\n' "## Findings template" '```json'
  python3 "$SCRIPT_DIR/review_findings.py" template --project "$PROJECT_ROOT" 2>/dev/null || printf '{"tree_hash": "%s"}\n' "$tree_hash"
  printf '%s\n' '```' "" "## Task contract"
  if [ -f "$contract" ]; then
    printf '%s\n' '```json'
    cat "$contract"
    printf '%s\n' '```'
  else
    printf '%s\n' "No task contract was recorded for this run (harness contract set FILE)."
  fi
  printf '\n%s\n%s\n' "## Verification (exit $verify_status)" '```'
  cat "$WORK/verify.log"
  printf '%s\n\n%s\n%s\n' '```' "## Diff" '```diff'
  cat "$WORK/patch.txt"
  printf '%s\n' '```'
} > "$packet.tmp.$$"
mv "$packet.tmp.$$" "$packet"

findings="$records_dir/review-findings.json"
info ""
info "Review packet: $packet"
if [ -n "${HARNESS_REVIEWER_CMD:-}" ]; then
  info "Running the independent reviewer (HARNESS_REVIEWER_CMD)."
  reviewer_status=0
  sh -c "$HARNESS_REVIEWER_CMD" harness-reviewer "$packet" "$WORK/findings.json" || reviewer_status=$?
  if [ "$reviewer_status" -ne 0 ] || [ ! -s "$WORK/findings.json" ]; then
    info "WARN: the reviewer exited $reviewer_status without findings; 'harness review done' will refuse until findings are submitted."
  else
    check_status=0
    python3 "$SCRIPT_DIR/review_findings.py" check --findings "$WORK/findings.json" --project "$PROJECT_ROOT" || check_status=$?
    if [ "$check_status" -le 1 ]; then
      cp "$WORK/findings.json" "$findings"
      info "Reviewer findings stored: $findings"
    fi
  fi
else
  info "Independent review: hand the packet to a reviewer in a fresh context (a subagent, or"
  info "claude -p / codex exec) that writes JSON per schemas/review-findings.schema.json, then"
  info "submit it: scripts/harness review submit FILE. 'harness review done' needs its approval."
fi

# Write the KEY=VALUE record that `scripts/harness review done` requires.
git_head=$(git -C "$PROJECT_ROOT" rev-parse HEAD 2>/dev/null || printf 'unknown')
record="$records_dir/review.state"
{
  printf 'RECORD_KIND=review\n'
  printf 'RUN_ID=%s\n' "$run_id"
  printf 'RECORD_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'RECORD_EPOCH=%s\n' "$(date +%s)"
  printf 'PROJECT_ROOT=%s\n' "$PROJECT_ROOT"
  printf 'GIT_HEAD=%s\n' "$git_head"
  printf 'TREE_HASH=%s\n' "$tree_hash"
  printf 'PACKET=%s\n' "$packet"
  printf 'VERIFY_EXIT=%s\n' "$verify_status"
  printf 'EXIT=%s\n' "$verify_status"
} > "$record.tmp.$$"
mv "$record.tmp.$$" "$record"
info ""
info "Run record: $record"
if command -v python3 >/dev/null 2>&1; then
  python3 "$SCRIPT_DIR/workflow_audit.py" check --kind review --record "$record" >&2 || :
fi

if [ "$verify_status" -ne 0 ]; then
  info "Review finished with verification failures (exit $verify_status)."
  exit "$verify_status"
fi
info "Review finished."
