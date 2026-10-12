#!/usr/bin/env sh
# scripts/jg.sh: refusals, pass-through, compact records, and the report,
# exercised against a fake `jg` on PATH so no provider is ever contacted.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
JG="$HARNESS_ROOT_UNDER_TEST/scripts/jg.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-jg.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)

PROJECT="$TMP_ROOT/project"
OUT="$TMP_ROOT/out.txt"
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_TARGET_ROOT || true
mkdir -p "$PROJECT/src" "$TMP_ROOT/bin"
printf '%s\n' 'print("hi")' > "$PROJECT/src/app.py"

fail() {
  printf '%s\n' "FAIL: $*"
  if [ -f "$OUT" ]; then
    printf '%s\n' "--- last output ---"
    cat "$OUT"
  fi
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

# run EXPECTED_EXIT ARGS...
run() {
  expected=$1
  shift
  status=0
  (cd "$PROJECT" && "$JG" "$@") > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "jg.sh $* exited $status, expected $expected"
}

records() {
  find "$HARNESS_DB_ROOT/retrieval" -name '*.state' 2>/dev/null | wc -l | tr -d ' '
}

# latest_record  The record written after the last `touch "$OUT.mark"`.
latest_record() {
  find "$HARNESS_DB_ROOT/retrieval" -name '*.state' -newer "$OUT.mark" | head -n 1
}

# --- without jg on PATH: install guidance, exit 2, no record -----------------

PATH_WITHOUT_JG=$(printf '%s' "$PATH" | tr ':' '\n' | while IFS= read -r dir; do
  [ -x "$dir/jg" ] || printf '%s:' "$dir"
done)
status=0
(cd "$PROJECT" && PATH="${PATH_WITHOUT_JG%:}" "$JG" "where is the entrypoint") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "missing jg should exit 2, got $status"
expect_output 'npm install --global @dzhng/jevgrep'
[ "$(records)" = "0" ] || fail "missing jg must not write a record"

# --- fake jg: echoes its arguments, honours FAKE_JG_EXIT ---------------------

cat > "$TMP_ROOT/bin/jg" <<'FAKE'
#!/usr/bin/env sh
printf 'fake jg question=%s root=%s extra=%s\n' "$1" "$2" "${3:-}"
printf 'Summary line\nsrc/app.py:1 excerpt\nEnd context.\n'
exit "${FAKE_JG_EXIT:-0}"
FAKE
chmod +x "$TMP_ROOT/bin/jg"
PATH="$TMP_ROOT/bin:$PATH"
export PATH

# --- usage ------------------------------------------------------------------

run 2
expect_output 'a question is required'
run 2 --no-cache
expect_output 'question must come before jg options'
run 2 --root /abs "q"
expect_output 'relative to the target root'
run 2 --root missing "q"
expect_output 'does not exist'
[ "$(records)" = "0" ] || fail "usage errors must not write records"

# --- refusals ---------------------------------------------------------------

run 4 "where is auth" --include-sensitive
expect_output 'REFUSED: --include-sensitive'
run 4 "where is auth" --no-cache --no-ignore
expect_output 'REFUSED: --no-ignore'
touch "$PROJECT/.harness-no-upload"
run 4 "where is auth"
expect_output 'REFUSED:.*\.harness-no-upload'
rm "$PROJECT/.harness-no-upload"
[ "$(records)" = "0" ] || fail "refusals must not write records"

# --- pass-through search with no active run ---------------------------------

run 0 "where is the entrypoint" --no-cache
expect_output "fake jg question=where is the entrypoint root=$PROJECT extra=--no-cache"
expect_output 'End context\.'
[ "$(records)" = "1" ] || fail "one record expected after a search, got $(records)"
record=$(find "$HARNESS_DB_ROOT/retrieval" -name '*.state')
for key in RECORD_KIND=jevgrep RUN_ID= PHASE=none SUBTREE=no EXIT=0 COMPLETE=yes; do
  grep -q "^$key" "$record" || fail "record missing $key"
done
grep -q '^QUESTION_SHA256=[0-9a-f]\{16,\}$' "$record" || fail "record missing the question hash"
grep -q '^DURATION_MS=[0-9]\{1,\}$' "$record" || fail "record missing the duration"
grep -q '^OUTPUT_BYTES=[1-9][0-9]*$' "$record" || fail "record missing the output size"
grep -q 'entrypoint' "$record" && fail "the question text must not be stored"
grep -q 'app.py' "$record" && fail "paths must not be stored"
[ "$(find "$HARNESS_DB_ROOT/retrieval" -maxdepth 0 -perm -700 ! -perm -077 | wc -l | tr -d ' ')" = "1" ] \
  || fail "retrieval directory must be private"

# --- subtree, active run, and incomplete/failed exits -----------------------

mkdir -p "$HARNESS_DB_ROOT/runs/run-1"
printf 'run-1\n' > "$HARNESS_DB_ROOT/runs/current"
printf 'RUN_ID=run-1\nRUN_STATUS=active\nCURRENT_PHASE=plan\n' > "$HARNESS_DB_ROOT/runs/run-1/state"
touch "$OUT.mark"; sleep 1
run 0 --root src "how is output written"
expect_output "root=$PROJECT/src"
latest=$(latest_record)
grep -q '^RUN_ID=run-1$' "$latest" || fail "record should carry the active run id"
grep -q '^PHASE=plan$' "$latest" || fail "record should carry the current phase"
grep -q '^SUBTREE=yes$' "$latest" || fail "record should flag a subtree search"

touch "$OUT.mark"; sleep 1
status=0
(cd "$PROJECT" && FAKE_JG_EXIT=2 "$JG" "partial") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "jg exit 2 should pass through, got $status"
latest=$(latest_record)
grep -q '^COMPLETE=partial$' "$latest" || fail "exit 2 should record a partial result"

touch "$OUT.mark"; sleep 1
status=0
(cd "$PROJECT" && FAKE_JG_EXIT=1 "$JG" "broken") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 1 ] || fail "jg exit 1 should pass through, got $status"
latest=$(latest_record)
grep -q '^COMPLETE=no$' "$latest" || fail "exit 1 should record an incomplete result"
[ "$(records)" = "4" ] || fail "four records expected, got $(records)"

# --- report -----------------------------------------------------------------

run 0 --report
expect_output 'Searches:     4 (complete: 2, failed: 2)'
expect_output 'Duration ms:  median [0-9]'
expect_output 'plan     3'
expect_output 'none     1'

# --- --project from elsewhere, and an empty report --------------------------

status=0
(cd "$TMP_ROOT" && "$JG" --project "$PROJECT" "from outside") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "--project search exited $status"
expect_output "root=$PROJECT"
status=0
(cd "$TMP_ROOT" && HARNESS_DB_ROOT="$TMP_ROOT/empty-db" "$JG" --project "$PROJECT" --report) > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "empty report exited $status"
expect_output 'No retrieval records yet'

printf '%s\n' 'PASS: jg.sh refuses widening flags and marked targets, passes output through, and records compact retrievals'
