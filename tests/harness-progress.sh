#!/usr/bin/env sh
set -eu

# progress.md holds only the current run. `harness plan start` archives the
# previous page under the run database and writes a fresh page for the new run.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CLI="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
unset HARNESS_ROOT HARNESS_PROGRESS_ROTATE
unset HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS HARNESS_BUDGET_TOKENS || true
HARNESS_JEV_CHECKPOINTS=0
export HARNESS_JEV_CHECKPOINTS
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-progress.XXXXXX")
cleanup() {
  chmod -R u+rwx "$TMP_ROOT" 2>/dev/null || :
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)

FIXTURE="$TMP_ROOT/fixture"
WORKDIR="$FIXTURE/src"
PAGE="$FIXTURE/progress.md"
OUT="$TMP_ROOT/out.txt"

fail() {
  printf '%s\n' "FAIL: $*"
  if [ -f "$OUT" ]; then
    printf '%s\n' "--- last output ---"
    cat "$OUT"
  fi
  exit 1
}

mkdir -p "$FIXTURE/scripts" "$WORKDIR"
printf '%s\n' '# fixture agent guide' > "$FIXTURE/AGENTS.md"
printf '%s\n' '#!/usr/bin/env sh' > "$FIXTURE/scripts/verify.sh"

case_number=0
new_case() {
  case_number=$((case_number + 1))
  HARNESS_DB_ROOT="$TMP_ROOT/db-$case_number"
  export HARNESS_DB_ROOT
  RUNS="$HARNESS_DB_ROOT/runs"
  rm -f "$PAGE"
}

run() {
  expected=$1
  shift
  status=0
  (cd "$WORKDIR" && "$CLI" "$@") > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "harness $* exited $status, expected $expected"
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

current_run() {
  head -n 1 "$RUNS/current"
}

expect_page_for() {
  [ "$(head -n 1 "$PAGE")" = "<!-- harness-run: $1 -->" ] || fail "progress.md does not name run $1: $(head -n 1 "$PAGE")"
  grep -q "^Run \`$1\`" "$PAGE" || fail "progress.md body does not name run $1"
  grep -q '^## Goal$' "$PAGE" || fail "progress.md page has no Goal section"
}

same_bytes() {
  cmp -s "$1" "$2" || fail "$3: $2 differs from the original"
}

# record KIND  Fake a passing scripts/verify.sh or scripts/review.sh record.
record() {
  mkdir -p "$HARNESS_DB_ROOT/records"
  printf '%s\n' "RECORD_KIND=$1" "RECORD_AT=fixture" "RECORD_EPOCH=$(date +%s)" "GIT_HEAD=fixture" "EXIT=0" \
    > "$HARNESS_DB_ROOT/records/$1.state"
}

complete_run() {
  run 0 plan "done"
  run 0 build start
  record verify
  run 0 build "done"
  run 0 review start
  record review
  run 0 review "done"
}

# --- a target without progress.md is left alone ------------------------------

new_case
run 0 plan start
[ ! -e "$PAGE" ] || fail "plan start created progress.md in a target that had none"
if grep -q 'Progress:' "$OUT"; then fail "plan start reported a rotation without progress.md"; fi

# --- legacy, unmarked history is kept with the run that rotated it -----------

new_case
printf '%s\n' '# Old diary' '' '## 2026-01-01 goal A' '- did A' '## 2026-02-01 goal B' '- did B' > "$PAGE"
cp "$PAGE" "$TMP_ROOT/legacy.orig"
run 0 plan start
first=$(current_run)
expect_output "Progress: archived the previous page to $RUNS/$first/progress.previous.md"
same_bytes "$TMP_ROOT/legacy.orig" "$RUNS/$first/progress.previous.md" "legacy archive"
expect_page_for "$first"
if grep -q 'goal A' "$PAGE"; then fail "old goals are still in progress.md"; fi

# --- a completed run's page moves to that run's directory --------------------

printf '%s\n' '- decision: rotate at plan start' >> "$PAGE"
cp "$PAGE" "$TMP_ROOT/first.orig"
complete_run
same_bytes "$TMP_ROOT/first.orig" "$PAGE" "review done must not touch progress.md"
run 0 plan start
second=$(current_run)
[ "$second" != "$first" ] || fail "plan start did not create a new run"
expect_output "archived the previous page to $RUNS/$first/progress.md"
same_bytes "$TMP_ROOT/first.orig" "$RUNS/$first/progress.md" "completed run archive"
same_bytes "$TMP_ROOT/legacy.orig" "$RUNS/$first/progress.previous.md" "earlier archive"
expect_page_for "$second"
if grep -q 'rotate at plan start' "$PAGE"; then fail "the finished run's decision is still in progress.md"; fi

# --- an aborted run is archived the same way; archives are never overwritten --

printf '%s\n' '- aborted run note' >> "$PAGE"
cp "$PAGE" "$TMP_ROOT/second.orig"
printf '%s\n' 'pre-existing archive' > "$RUNS/$second/progress.md"
run 0 abort "Test: finish the run."
run 0 plan start
third=$(current_run)
expect_output "archived the previous page to $RUNS/$second/progress.1.md"
[ "$(cat "$RUNS/$second/progress.md")" = "pre-existing archive" ] || fail "an existing archive was overwritten"
same_bytes "$TMP_ROOT/second.orig" "$RUNS/$second/progress.1.md" "aborted run archive"
expect_page_for "$third"

# --- a refused plan start changes nothing -----------------------------------

cp "$PAGE" "$TMP_ROOT/third.orig"
run 4 plan start
same_bytes "$TMP_ROOT/third.orig" "$PAGE" "refused plan start"
[ "$(current_run)" = "$third" ] || fail "a refused plan start changed the current run"

# --- a marker for an unknown run or a traversal falls back to the new run ----

for marker in 'nope-20990101T000000Z-1' '..' '../escape'; do
  new_case
  mkdir -p "$TMP_ROOT/escape"
  printf '%s\n' "<!-- harness-run: $marker -->" '# someone else' > "$PAGE"
  cp "$PAGE" "$TMP_ROOT/marker.orig"
  run 0 plan start
  new=$(current_run)
  same_bytes "$TMP_ROOT/marker.orig" "$RUNS/$new/progress.previous.md" "marker '$marker'"
  [ ! -e "$TMP_ROOT/escape/progress.md" ] || fail "marker '$marker' wrote outside the run database"
  expect_page_for "$new"
done

# --- an empty page is replaced without an archive ----------------------------

new_case
: > "$PAGE"
run 0 plan start
new=$(current_run)
expect_output "progress.md was empty"
[ ! -e "$RUNS/$new/progress.previous.md" ] || fail "an empty page was archived"
expect_page_for "$new"

# --- the page keeps its mode ------------------------------------------------

new_case
printf '%s\n' 'private notes' > "$PAGE"
chmod 600 "$PAGE"
run 0 plan start
mode=$(stat -c %a "$PAGE" 2>/dev/null || stat -f %Lp "$PAGE")
[ "$mode" = "600" ] || fail "progress.md mode changed to $mode"

# --- a symlinked page is left alone ------------------------------------------

new_case
printf '%s\n' 'shared page' > "$TMP_ROOT/elsewhere.md"
ln -s "$TMP_ROOT/elsewhere.md" "$PAGE"
run 0 plan start
expect_output "is a symlink; it was not archived or replaced"
[ -L "$PAGE" ] || fail "the symlink was replaced"
[ "$(cat "$TMP_ROOT/elsewhere.md")" = "shared page" ] || fail "the symlink target was rewritten"
rm -f "$PAGE"

# --- opting out keeps the page -----------------------------------------------

new_case
printf '%s\n' 'keep me' > "$PAGE"
(HARNESS_PROGRESS_ROTATE=0; export HARNESS_PROGRESS_ROTATE; run 0 plan start)
[ "$(cat "$PAGE")" = "keep me" ] || fail "HARNESS_PROGRESS_ROTATE=0 did not keep progress.md"
[ ! -e "$RUNS/$(current_run)/progress.previous.md" ] || fail "HARNESS_PROGRESS_ROTATE=0 still archived"

# --- a failed archive leaves the page unchanged and the run started ----------

# A copy that reports success but is short must not count as an archive.
new_case
mkdir -p "$TMP_ROOT/shim"
cat > "$TMP_ROOT/shim/cp" <<'EOF'
#!/bin/sh
for last; do :; done
printf 'partial\n' > "$last"
EOF
chmod +x "$TMP_ROOT/shim/cp"
printf '%s\n' 'line one' 'line two' > "$PAGE"
cp "$PAGE" "$TMP_ROOT/short.orig"
(PATH="$TMP_ROOT/shim:$PATH"; export PATH; run 0 plan start)
expect_output "WARN: could not archive"
same_bytes "$TMP_ROOT/short.orig" "$PAGE" "short copy"
[ ! -e "$RUNS/$(current_run)/progress.previous.md" ] || fail "a short copy was kept as an archive"

if [ "$(id -u)" -ne 0 ]; then
  new_case
  run 0 plan start
  owner=$(current_run)
  run 0 abort "Test: finish the run."
  printf '%s\n' "<!-- harness-run: $owner -->" 'notes that must survive' > "$PAGE"
  cp "$PAGE" "$TMP_ROOT/locked.orig"
  chmod 555 "$RUNS/$owner"
  run 0 plan start
  chmod 755 "$RUNS/$owner"
  expect_output "WARN: could not archive"
  same_bytes "$TMP_ROOT/locked.orig" "$PAGE" "failed archive"
  [ ! -e "$RUNS/$owner/progress.md" ] || fail "a failed archive left a file behind"
  [ "$(current_run)" != "$owner" ] || fail "the run did not start after a failed archive"

  new_case
  printf '%s\n' 'unreadable notes' > "$PAGE"
  chmod 000 "$PAGE"
  run 0 plan start
  chmod 644 "$PAGE"
  expect_output "WARN: could not archive"
  [ "$(cat "$PAGE")" = "unreadable notes" ] || fail "an unreadable page was changed"
  [ ! -e "$RUNS/$(current_run)/progress.previous.md" ] || fail "an unreadable page left a partial archive"
fi

printf '%s\n' 'PASS: harness progress.md holds the current run; finished pages are archived without loss'
