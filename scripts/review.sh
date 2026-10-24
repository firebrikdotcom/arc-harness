#!/usr/bin/env sh
set -u

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
CLI_ROOT=${HARNESS_ROOT:-}
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
# Record where the harness CLI reads gate records, as scripts/verify.sh does.
STATE_ROOT=$HARNESS_ROOT
if [ -n "$CLI_ROOT" ] && [ -f "$CLI_ROOT/AGENTS.md" ] && [ -f "$CLI_ROOT/scripts/verify.sh" ]; then
  STATE_ROOT=$(CDPATH='' cd "$CLI_ROOT" && pwd -P)
fi
HARNESS_DB_ROOT="${HARNESS_DB_ROOT:-$STATE_ROOT/.harness-db}"
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/review.sh [--project PATH]"
  info ""
  info "Runs project verification, prints the target/harness diff, and writes a review"
  info "packet (task contract, verification output, diff) for an independent reviewer."
  info "Each acceptance criterion of the run's contract is printed, in the terminal and"
  info "the packet, for the reviewer to answer as met, not_met, or not_applicable."
  info "When a person has written a reviewer command into .harness-db/reviewer (one"
  info "line; the guard keeps agents from writing it), review.sh runs that reviewer:"
  info "  sh -c \"\$(cat .harness-db/reviewer)\" reviewer PACKET FINDINGS_OUT"
  info "It runs in an empty scratch directory outside the project and harness, with the"
  info "packet on stdin and without agent-session or HARNESS_* variables."
  info "Independent reviews can take several minutes; when a reviewer is configured,"
  info "run review.sh in the background or with a long tool timeout."
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
BASE_DB_ROOT=$HARNESS_DB_ROOT
if [ -f "$BASE_DB_ROOT/review-off" ]; then
  info "Review is off ($BASE_DB_ROOT/review-off exists); no packet written and no reviewer started."
  exit 0
fi

# A registered target (scripts/harness-target.sh) owns its own database
# beneath the database root.
if [ -x "$SCRIPT_DIR/harness-target.sh" ]; then
  target_db=$(HARNESS_DB_ROOT=$HARNESS_DB_ROOT "$SCRIPT_DIR/harness-target.sh" db-root "$PROJECT_ROOT" 2>/dev/null || :)
  if [ -n "$target_db" ]; then
    HARNESS_DB_ROOT=$target_db
  fi
fi

# The reviewer command is read once, before verification runs project code, and
# that snapshot is what runs; its hash goes into the review record.
reviewer_cmd=$(head -n 1 "$BASE_DB_ROOT/reviewer" 2>/dev/null || :)
reviewer_sha="none"
if [ -n "$reviewer_cmd" ]; then
  if command -v sha256sum >/dev/null 2>&1; then
    reviewer_sha=$(printf '%s\n' "$reviewer_cmd" | sha256sum | cut -d' ' -f1)
  elif command -v shasum >/dev/null 2>&1; then
    reviewer_sha=$(printf '%s\n' "$reviewer_cmd" | shasum -a 256 | cut -d' ' -f1)
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
REVIEWER_DIR=""
trap 'rm -rf "$WORK" ${REVIEWER_DIR:+"$REVIEWER_DIR"}' EXIT HUP INT TERM

# A passing verify run of this run on exactly these files already proved the
# checks, so it is reused instead of running them twice; HARNESS_REVIEW_REVERIFY=1
# forces a fresh run.
reused_verify=""
verify_record="$(sh "$SCRIPT_DIR/run-paths.sh" records "$HARNESS_DB_ROOT")/verify.state"
verify_run=$(head -n 1 "$(sh "$SCRIPT_DIR/run-paths.sh" current "$HARNESS_DB_ROOT")" 2>/dev/null || :)
record_field() { sed -n "s/^$1=//p" "$verify_record" | tail -n 1; }
if [ "${HARNESS_REVIEW_REVERIFY:-0}" != "1" ] && [ -f "$verify_record" ] && [ -n "$verify_run" ] \
  && [ "$(record_field EXIT)" = "0" ] && [ "$(record_field FAILURES)" = "0" ] \
  && [ "$(record_field RUN_ID)" = "$verify_run" ] && [ "$(record_field PROJECT_ROOT)" = "$PROJECT_ROOT" ]; then
  verify_tree=$(record_field TREE_HASH)
  if [ -n "$verify_tree" ] && [ "$verify_tree" != "none" ] && [ "$verify_tree" = "$(sh "$SCRIPT_DIR/tree-hash.sh" "$PROJECT_ROOT")" ]; then
    reused_verify=$(record_field RECORD_AT)
  fi
fi

if [ -n "$reused_verify" ]; then
  {
    info "Verification reused: the passing run at $reused_verify covers these exact files (HARNESS_REVIEW_REVERIFY=1 reruns it)."
    info "Checks passed: $(record_field PASSED_CHECKS)"
    info "Verification summary: ran=$(record_field RAN) skipped=$(record_field SKIPPED) failures=0 scope=$(record_field SCOPE)"
  } | tee "$WORK/verify.log"
  verify_status=0
else
  # Verification failing is exactly when a reviewer needs the evidence below,
  # so its status is captured and the review continues.
  { "$SCRIPT_DIR/verify.sh" --project "$PROJECT_ROOT"; printf '%s\n' "$?" > "$WORK/verify.status"; } 2>&1 | tee "$WORK/verify.log"
  verify_status=$(cat "$WORK/verify.status" 2>/dev/null || printf '1')
fi
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

# Every acceptance criterion goes to the reviewer to answer. A criterion with a
# command already passed at build done; one without a command is checked by no
# one else, so the reviewer is its only check. With a contract, the positional
# parameters (already parsed above) carry "--contract PATH" to review_findings.py.
set --
if [ -n "$run_id" ] && [ -f "$contract" ]; then
  set -- --contract "$contract"
  criteria=$(python3 "$SCRIPT_DIR/task_contract.py" checklist "$contract" 2>&1) || criteria="The task contract could not be read: $criteria"
else
  criteria="No task contract was recorded for this run (harness contract set FILE); judge the change against its stated goal."
fi
info ""
info "==> Acceptance criteria"
printf '%s\n' "$criteria"
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
  printf '%s\n' "verdict \"block\" if any blocker or major finding is open or any acceptance criterion"
  printf '%s\n' "is not met, otherwise \"approve\"." ""
  printf 'Project: %s\nTree hash: %s\n\n' "$PROJECT_ROOT" "$tree_hash"
  printf '%s\n' "## Findings template" '```json'
  python3 "$SCRIPT_DIR/review_findings.py" template --project "$PROJECT_ROOT" "$@" 2>/dev/null || printf '{"tree_hash": "%s"}\n' "$tree_hash"
  printf '%s\n' '```' "" "## Acceptance criteria" "$criteria" "" "## Task contract"
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

# reviewer_scratch  Makes an empty private directory for the reviewer outside the
# project and the harness, so it discovers neither one's guides, settings, or
# hooks. TMPDIR comes last: it may point inside either root.
reviewer_scratch() {
  for parent in /tmp /var/tmp "${TMPDIR:-}"; do
    if [ -z "$parent" ] || [ ! -d "$parent" ]; then
      continue
    fi
    parent=$(CDPATH='' cd "$parent" 2>/dev/null && pwd -P) || continue
    case "$parent/" in
      "$PROJECT_ROOT"/*|"$HARNESS_ROOT"/*) continue ;;
    esac
    mktemp -d "$parent/harness-reviewer.XXXXXX" 2>/dev/null && return 0
  done
  return 1
}

# run_reviewer DIR  Runs the configured reviewer as a fresh process: from DIR,
# with the packet on stdin, and without the author's session. Agent-session
# variables would tie its hooks and bookkeeping to the author's run, and harness
# variables carry the author's state, so they are dropped; HARNESS_AUTO_INIT=0
# keeps the SessionStart hook from registering DIR. Everything else, such as an
# API key, passes through, and the command line itself can set a dropped variable.
run_reviewer() {
  (
    for name in $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p'); do
      case "$name" in
        CLAUDECODE|CLAUDE_CODE_*|CLAUDE_SESSION_ID|CLAUDE_PROJECT_DIR|CLAUDE_ENV_FILE|CODEX_*|HARNESS_*)
          unset "$name" 2>/dev/null || :
          ;;
      esac
    done
    HARNESS_AUTO_INIT=0
    export HARNESS_AUTO_INIT
    cd "$1" || exit 1
    # shellcheck disable=SC2094 # the reviewer only reads REVIEW.md; it writes findings.json
    exec sh -c "$reviewer_cmd" harness-reviewer "$1/REVIEW.md" "$1/findings.json" < "$1/REVIEW.md"
  )
}

if [ -n "$reviewer_cmd" ]; then
  info "Running the independent reviewer ($BASE_DB_ROOT/reviewer, sha256 $reviewer_sha)."
  reviewer_status=0
  if REVIEWER_DIR=$(reviewer_scratch) && cp "$packet" "$REVIEWER_DIR/REVIEW.md"; then
    run_reviewer "$REVIEWER_DIR" || reviewer_status=$?
    if [ -f "$REVIEWER_DIR/findings.json" ]; then
      cp "$REVIEWER_DIR/findings.json" "$WORK/findings.json"
    fi
    rm -rf "$REVIEWER_DIR"
    REVIEWER_DIR=""
  else
    info "WARN: no scratch directory outside the project and harness could be made; the reviewer was not started."
    reviewer_status=1
  fi
  if [ "$reviewer_status" -ne 0 ] || [ ! -s "$WORK/findings.json" ]; then
    info "WARN: the reviewer exited $reviewer_status without findings; 'harness review done' will refuse until findings are submitted."
  else
    check_status=0
    python3 "$SCRIPT_DIR/review_findings.py" check --findings "$WORK/findings.json" --project "$PROJECT_ROOT" "$@" || check_status=$?
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
  printf 'REVIEWER_COMMAND_SHA256=%s\n' "$reviewer_sha"
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
