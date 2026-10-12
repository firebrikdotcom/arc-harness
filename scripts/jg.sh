#!/usr/bin/env sh
# Semantic source retrieval for a harness target through jevgrep (jg).
#
#   scripts/jg.sh [--project PATH] [--root SUBDIR] "question" [jg options...]
#   scripts/jg.sh [--project PATH] --report
#
# jg sends eligible source of the searched tree to the provider chosen with
# `jg auth`, so this wrapper is the sanctioned way for an agent to call it
# inside the harness:
#   - it resolves the target root (default: the current directory) and searches
#     that root, or a subtree given as `--root SUBDIR` relative to it;
#   - it refuses when the target root contains a `.harness-no-upload` marker;
#   - it refuses the upload-widening flags `--include-sensitive` and
#     `--no-ignore`, which the default denylist also rejects;
#   - it passes jg's output through unchanged and writes one compact record
#     under the target database (`retrieval/<stamp>.state`): time, run id and
#     phase, a sha256 of the question, duration, exit code, output size, and
#     whether the output was complete. The question text, paths, excerpts and
#     source never enter the record.
# Exit 2 for usage or a missing `jg`, 4 for a refusal, otherwise jg's exit code
# (0 complete, 1 failed, 2 incomplete, 130 interrupted).
set -u

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HARNESS_DB_ROOT="${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}"
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"
NO_UPLOAD_MARKER=".harness-no-upload"
EXIT_USAGE=2
EXIT_REFUSED=4

info() {
  printf '%s\n' "$*"
}

usage() {
  sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

now_ms() {
  if has_cmd python3; then
    python3 -c 'import time; print(int(time.time() * 1000))'
  else
    printf '%s000\n' "$(date +%s)"
  fi
}

hash_stdin() {
  if has_cmd sha256sum; then
    sha256sum | cut -d' ' -f1
  elif has_cmd shasum; then
    shasum -a 256 | cut -d' ' -f1
  else
    cksum | cut -d' ' -f1
  fi
}

# median FILE  Median of one integer per line; empty input prints "unknown".
median() {
  sort -n "$1" | awk '{ v[NR] = $1 } END {
    if (NR == 0) { print "unknown"; exit }
    if (NR % 2) { print v[(NR + 1) / 2] } else { printf "%d\n", (v[NR / 2] + v[NR / 2 + 1]) / 2 }
  }'
}

mode=search
question=""
subtree=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project)
      [ "$#" -ge 2 ] || { info "FAIL: --project requires a path."; exit "$EXIT_USAGE"; }
      PROJECT_ROOT=$2
      shift 2
      ;;
    --root)
      [ "$#" -ge 2 ] || { info "FAIL: --root requires a directory relative to the target root."; exit "$EXIT_USAGE"; }
      subtree=$2
      shift 2
      ;;
    --report)
      mode=report
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      question=$1
      shift
      break
      ;;
  esac
done

if [ ! -d "$PROJECT_ROOT" ]; then
  info "FAIL: project root does not exist or is not a directory: $PROJECT_ROOT"
  exit "$EXIT_USAGE"
fi
PROJECT_ROOT=$(CDPATH='' cd "$PROJECT_ROOT" && pwd -P)

# A registered target (scripts/harness-target.sh) owns its own database.
if [ -x "$SCRIPT_DIR/harness-target.sh" ]; then
  target_db=$(HARNESS_DB_ROOT=$HARNESS_DB_ROOT "$SCRIPT_DIR/harness-target.sh" db-root "$PROJECT_ROOT" 2>/dev/null || :)
  if [ -n "$target_db" ]; then
    HARNESS_DB_ROOT=$target_db
  fi
fi
RECORDS_DIR="$HARNESS_DB_ROOT/retrieval"

if [ "$mode" = "report" ]; then
  info "Retrieval report"
  info "Project root: $PROJECT_ROOT"
  info "Records:      $RECORDS_DIR"
  if [ ! -d "$RECORDS_DIR" ] || [ -z "$(ls -A "$RECORDS_DIR" 2>/dev/null)" ]; then
    info "No retrieval records yet."
    exit 0
  fi
  total=0
  complete=0
  failed=0
  durations=$(mktemp "${TMPDIR:-/tmp}/jg-report.XXXXXX")
  phases=$(mktemp "${TMPDIR:-/tmp}/jg-phases.XXXXXX")
  trap 'rm -f "$durations" "$phases"' EXIT HUP INT TERM
  for record in "$RECORDS_DIR"/*.state; do
    [ -f "$record" ] || continue
    total=$((total + 1))
    grep -q '^COMPLETE=yes$' "$record" && complete=$((complete + 1))
    grep -q '^EXIT=0$' "$record" || failed=$((failed + 1))
    sed -n 's/^DURATION_MS=//p' "$record" >> "$durations"
    sed -n 's/^PHASE=//p' "$record" >> "$phases"
  done
  info "Searches:     $total (complete: $complete, failed: $failed)"
  info "Duration ms:  median $(median "$durations")"
  info "By phase:"
  sort "$phases" | uniq -c | awk '{ printf "  %-8s %s\n", $2, $1 }'
  exit 0
fi

if [ -z "$question" ] || [ "$question" = "--" ]; then
  info "FAIL: a question is required."
  usage
  exit "$EXIT_USAGE"
fi
case "$question" in
  --*)
    info "FAIL: the question must come before jg options: scripts/jg.sh \"question\" $question"
    exit "$EXIT_USAGE"
    ;;
esac

for argument in "$@"; do
  case "$argument" in
    --include-sensitive|--no-ignore)
      info "REFUSED: $argument would upload credential-shaped or ignored files; the harness does not pass it to jg."
      exit "$EXIT_REFUSED"
      ;;
  esac
done

if [ -e "$PROJECT_ROOT/$NO_UPLOAD_MARKER" ]; then
  info "REFUSED: $PROJECT_ROOT contains $NO_UPLOAD_MARKER; its source must not be sent to a retrieval provider."
  exit "$EXIT_REFUSED"
fi

if ! has_cmd jg; then
  info "FAIL: jg is not installed. Install it with: npm install --global @dzhng/jevgrep@latest"
  info "Then ask the user to run 'jg auth' and 'jg doctor' in their terminal; the key is never entered in chat."
  exit "$EXIT_USAGE"
fi

search_root=$PROJECT_ROOT
if [ -n "$subtree" ]; then
  case "$subtree" in
    /*|*..*)
      info "FAIL: --root must be a directory relative to the target root without '..'."
      exit "$EXIT_USAGE"
      ;;
  esac
  search_root="$PROJECT_ROOT/$subtree"
  if [ ! -d "$search_root" ]; then
    info "FAIL: --root directory does not exist: $search_root"
    exit "$EXIT_USAGE"
  fi
fi

run_id=""
phase="none"
current_file=$(sh "$SCRIPT_DIR/run-paths.sh" current "$HARNESS_DB_ROOT")
if [ -f "$current_file" ]; then
  run_id=$(head -n 1 "$current_file" 2>/dev/null || :)
  if [ -n "$run_id" ] && [ -f "$HARNESS_DB_ROOT/runs/$run_id/state" ]; then
    phase=$(sed -n 's/^CURRENT_PHASE=//p' "$HARNESS_DB_ROOT/runs/$run_id/state" | tail -n 1)
    [ -n "$phase" ] || phase=none
  fi
fi

output=$(mktemp "${TMPDIR:-/tmp}/jg-output.XXXXXX")
status_file="$output.status"
trap 'rm -f "$output" "$status_file"' EXIT HUP INT TERM
started_ms=$(now_ms)
# Stream jg's output while keeping its exit status (POSIX sh has no PIPESTATUS).
{ jg "$question" "$search_root" "$@" 2>&1; printf '%s\n' "$?" > "$status_file"; } | tee "$output"
jg_status=$(cat "$status_file" 2>/dev/null || printf '1\n')
finished_ms=$(now_ms)

case "$jg_status" in
  0) complete=yes ;;
  2) complete=partial ;;
  *) complete=no ;;
esac
output_bytes=$(wc -c < "$output" | tr -d ' ')
question_hash=$(printf '%s' "$question" | hash_stdin)

mkdir -p "$RECORDS_DIR" 2>/dev/null || :
chmod 700 "$RECORDS_DIR" 2>/dev/null || :
record="$RECORDS_DIR/$(date -u +%Y%m%dT%H%M%SZ)-$$.state"
umask 077
{
  printf 'RECORD_KIND=jevgrep\n'
  printf 'RECORD_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'RECORD_EPOCH=%s\n' "$(date +%s)"
  printf 'RUN_ID=%s\n' "$run_id"
  printf 'PHASE=%s\n' "$phase"
  printf 'SUBTREE=%s\n' "$([ -n "$subtree" ] && printf 'yes' || printf 'no')"
  printf 'QUESTION_SHA256=%s\n' "$question_hash"
  printf 'DURATION_MS=%s\n' "$((finished_ms - started_ms))"
  printf 'EXIT=%s\n' "$jg_status"
  printf 'OUTPUT_BYTES=%s\n' "$output_bytes"
  printf 'COMPLETE=%s\n' "$complete"
} > "$record" 2>/dev/null || info "WARN: could not write the retrieval record under $RECORDS_DIR."

exit "$jg_status"
