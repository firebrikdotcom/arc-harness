#!/usr/bin/env sh
# One passing verify run proves its checks once: review.sh and a `make` acceptance
# criterion that only repeats them reuse it for the same run and the same files,
# and run again as soon as anything differs.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
unset HARNESS_SESSION_ID CODEX_THREAD_ID CLAUDE_SESSION_ID CLAUDE_CODE_SESSION_ID HARNESS_REVIEWER_CMD HARNESS_REVIEW_BASE HARNESS_REVIEW_REVERIFY || true
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-verify-reuse.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
export HARNESS_DB_ROOT="$TMP_ROOT/db"
PROJECT="$TMP_ROOT/project"
OUT="$TMP_ROOT/out.txt"
# Every recipe appends to a log outside the project, so runs are counted without changing the files under test.
export RUNS="$TMP_ROOT/runs.log"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}
runs() { grep -c "^$1\$" "$RUNS" 2>/dev/null || printf '0\n'; }

mkdir -p "$PROJECT" "$HARNESS_DB_ROOT/runs"
printf 'run-1\n' > "$HARNESS_DB_ROOT/runs/current"
cat > "$PROJECT/Makefile" <<'MAKE'
test:
	@echo test >> "$(RUNS)"
build:
	@echo build >> "$(RUNS)"
verify: test build
e2e:
	@echo e2e >> "$(RUNS)"
wider: test build e2e
MAKE
printf 'one\n' > "$PROJECT/file.txt"
git -C "$PROJECT" init -q
git -C "$PROJECT" add Makefile file.txt
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m init

"$ROOT/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
grep -q '^PASSED_CHECKS=make:test make:build$' "$HARNESS_DB_ROOT/records/verify.state" || fail "verify record does not name its passed checks"
[ "$(runs test)" = 1 ] || fail "verify did not run make test once"

# 1. review.sh reuses the passing run of this run on the same files.
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "review failed"
grep -q '^Verification reused: ' "$OUT" || fail "review did not reuse the passing verify run"
grep -q '^Checks passed: make:test make:build$' "$OUT" || fail "review did not name the reused checks"
grep -q 'Verification reused: ' "$HARNESS_DB_ROOT/records/review-packet.md" || fail "the reviewer packet does not say verification was reused"
grep -q '^EXIT=0$' "$HARNESS_DB_ROOT/records/review.state" || fail "review record should pass"
[ "$(runs test)" = 1 ] || fail "review ran the checks again"

# 2. Acceptance: make verify repeats only what verify ran, so it is counted from that run; e2e still runs.
cat > "$TMP_ROOT/task.json" <<'JSON'
{"goal": "g", "deliverables": ["d"], "non_goals": [], "acceptance": [
 {"id": "checks", "criterion": "Local checks pass", "command": ["make", "verify"]},
 {"id": "wider", "criterion": "Checks plus e2e", "command": ["make", "wider"]},
 {"id": "ui", "criterion": "UI suite", "command": ["make", "e2e"]}]}
JSON
criteria() { python3 "$ROOT/scripts/task_contract.py" criteria "$TMP_ROOT/task.json" --project "$PROJECT" "$@" > "$OUT" 2>&1; }
criteria --verified "$HARNESS_DB_ROOT/records/verify.state" || fail "criteria failed"
grep -q '^PASS: checks: Local checks pass (same checks as the passing verify run at ' "$OUT" || fail "make verify was not counted from the verify run"
[ "$(runs test)" = 2 ] || fail "only make wider should have run make test again (runs: $(runs test))"
[ "$(runs e2e)" = 2 ] || fail "e2e and wider must each run e2e (runs: $(runs e2e))"
# Without a record every command runs.
criteria || fail "criteria without a record failed"
[ "$(runs test)" = 4 ] || fail "without a record make verify and make wider must run (runs: $(runs test))"

# 3. Any change to the files makes both run again.
printf 'two\n' > "$PROJECT/file.txt"
criteria --verified "$HARNESS_DB_ROOT/records/verify.state" || fail "criteria after an edit failed"
grep -q 'not run again' "$OUT" && fail "an edited tree reused the old verify run"
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "review after an edit failed"
grep -q '^Verification reused: ' "$OUT" && fail "review reused a verify run of different files"
before=$(runs test)

# 4. HARNESS_REVIEW_REVERIFY=1 forces a fresh run even when the record matches.
HARNESS_REVIEW_REVERIFY=1 "$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "forced review failed"
grep -q '^Verification reused: ' "$OUT" && fail "HARNESS_REVIEW_REVERIFY=1 still reused"
[ "$(runs test)" -gt "$before" ] || fail "HARNESS_REVIEW_REVERIFY=1 did not run the checks"

# 5. Another run's record, or a failing one, is never reused.
printf 'run-2\n' > "$HARNESS_DB_ROOT/runs/current"
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "review in a new run failed"
grep -q '^Verification reused: ' "$OUT" && fail "review reused another run's verify record"
sed -i.bak 's/^EXIT=0$/EXIT=1/' "$HARNESS_DB_ROOT/records/verify.state"
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || :
grep -q '^Verification reused: ' "$OUT" && fail "review reused a failing verify record"

# 6. A same-size edit in the same second as the commit still changes the tree hash
# (git re-reads such files only while the index copy keeps the index's time).
i=0
while [ "$i" -lt 25 ]; do
  racy="$TMP_ROOT/racy-$i"
  mkdir "$racy"
  git -C "$racy" init -q
  printf 'one\n' > "$racy/file.txt"
  git -C "$racy" add file.txt
  git -C "$racy" -c user.email=t@example.com -c user.name=t commit -q -m init
  before=$(sh "$ROOT/scripts/tree-hash.sh" "$racy")
  printf 'two\n' > "$racy/file.txt"
  [ "$(sh "$ROOT/scripts/tree-hash.sh" "$racy")" != "$before" ] || fail "tree hash missed a same-second same-size edit (try $i)"
  i=$((i + 1))
done

printf '%s\n' 'PASS: a passing verify run is reused by review and identical acceptance checks, and only for the same files'
