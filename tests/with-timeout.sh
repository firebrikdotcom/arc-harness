#!/usr/bin/env sh
# Bootstrap and verification commands run under a time limit: a hung command
# and its child processes are stopped, it fails with exit 124 and a TIMEOUT:
# line, and the rest of verification still runs.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
WRAPPER="$HARNESS_ROOT_UNDER_TEST/scripts/with-timeout.sh"
INIT="$HARNESS_ROOT_UNDER_TEST/scripts/init.sh"
VERIFY="$HARNESS_ROOT_UNDER_TEST/scripts/verify.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-timeout.XXXXXX")
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
trap 'rm -rf "${TMP_ROOT:?}"' EXIT HUP INT TERM
OUT="$TMP_ROOT/out.txt"
KID="$TMP_ROOT/kid.pid"
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_REQUIRED_CHECKS HARNESS_BOOTSTRAP_TIMEOUT HARNESS_VERIFY_TIMEOUT
# One second of grace keeps the escalation-to-KILL cases short.
HARNESS_TIMEOUT_GRACE=1
export HARNESS_TIMEOUT_GRACE

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

# A command that hangs and leaves a background child that ignores TERM, so
# only a KILL to the whole process group stops everything.
HANG="$TMP_ROOT/hang.sh"
cat > "$HANG" <<EOF
#!/bin/sh
trap '' TERM
sleep 60 &
printf '%s\n' "\$!" > "$KID"
sleep 60
EOF
chmod +x "$HANG"

# expect_group_stopped  The hung command's background child is gone too.
expect_group_stopped() {
  [ -s "$KID" ] || fail "the hung command never started its child"
  _pid=$(cat "$KID")
  _tries=0
  while kill -0 "$_pid" 2>/dev/null; do
    _tries=$((_tries + 1))
    [ "$_tries" -lt 20 ] || fail "child process $_pid survived the timeout"
    sleep 0.1
  done
  rm -f "$KID"
}

# timed CMD...  Runs CMD with output in $OUT; sets STATUS and ELAPSED.
timed() {
  _start=$(date +%s)
  STATUS=0
  "$@" > "$OUT" 2>&1 < /dev/null || STATUS=$?
  ELAPSED=$(( $(date +%s) - _start ))
}

# PATH without the named tools, so each supervisor can be tested on a machine
# that has all of them installed.
path_without() {
  _bin="$TMP_ROOT/bin-without-$(printf '%s' "$1" | tr ' ' '-')"
  mkdir -p "$_bin"
  _old_ifs=$IFS
  IFS=:
  for dir in $PATH; do
    [ -d "$dir" ] || continue
    for tool in "$dir"/*; do
      if [ ! -f "$tool" ] || [ ! -x "$tool" ]; then
        continue
      fi
      name=${tool##*/}
      case " $1 " in
        *" $name "*) continue ;;
      esac
      [ -e "$_bin/$name" ] || ln -s "$tool" "$_bin/$name"
    done
  done
  IFS=$_old_ifs
  printf '%s\n' "$_bin"
}

# check_supervisor NAME PATH  The wrapper's contract under one supervisor.
check_supervisor() {
  _name=$1
  _path=$2
  timed env PATH="$_path" "$WRAPPER" 1 "hang ($_name)" DEMO_TIMEOUT "$HANG"
  [ "$STATUS" -eq 124 ] || fail "$_name: a hung command exited $STATUS, expected 124"
  [ "$ELAPSED" -le 6 ] || fail "$_name: the timeout took ${ELAPSED}s for a 1s limit"
  expect_output "TIMEOUT: hang ($_name) did not finish within 1s"
  expect_output 'Set DEMO_TIMEOUT to allow more time'
  expect_group_stopped

  # A command that finishes in time keeps its own status and prints nothing extra.
  timed env PATH="$_path" "$WRAPPER" 5 quick DEMO_TIMEOUT sh -c 'echo done; exit 7'
  [ "$STATUS" -eq 7 ] || fail "$_name: a quick command's exit 7 became $STATUS"
  [ "$(cat "$OUT")" = 'done' ] || fail "$_name: a quick command's output changed"
  timed env PATH="$_path" "$WRAPPER" 5 quick DEMO_TIMEOUT sh -c 'exit 124'
  [ "$STATUS" -eq 124 ] || fail "$_name: a command's own exit 124 became $STATUS"
  if grep -q TIMEOUT "$OUT"; then
    fail "$_name: a command that exited 124 on its own was reported as a timeout"
  fi
}

if command -v python3 >/dev/null 2>&1; then
  check_supervisor python3 "$PATH"
fi
# Only python3 is hidden, so GNU timeout is on this PATH exactly when it is on ours.
for tool in timeout gtimeout; do
  if command -v "$tool" >/dev/null 2>&1 && "$tool" --version 2>/dev/null | head -n 1 | grep -q 'GNU coreutils'; then
    check_supervisor "GNU timeout" "$(path_without "python3")"
    break
  fi
done

# With no supervisor at all the command is not run: exit 125, and the gap is named.
NO_TOOLS=$(path_without "python3 timeout gtimeout")
MARKER="$TMP_ROOT/unbounded-ran"
timed env PATH="$NO_TOOLS" "$WRAPPER" 5 unbounded DEMO_TIMEOUT touch "$MARKER"
[ "$STATUS" -eq 125 ] || fail "without a supervisor the wrapper exited $STATUS, expected 125"
[ ! -e "$MARKER" ] || fail "the command ran without a supervisor"
expect_output 'FAIL: unbounded cannot be time-limited: no timeout supervisor'
expect_output 'Nothing was run'
env PATH="$NO_TOOLS" "$WRAPPER" --check && fail "--check passed without a supervisor"
"$WRAPPER" --check || fail "--check failed although a supervisor is installed"

# Limits must be whole seconds above zero.
for bad in 0 -1 abc 1.5 ''; do
  timed "$WRAPPER" "$bad" label DEMO_TIMEOUT true
  [ "$STATUS" -eq 2 ] || fail "limit '$bad' exited $STATUS, expected 2"
  expect_output 'DEMO_TIMEOUT must be a whole number of seconds greater than 0'
done
for bad in 0 abc 1.5; do
  timed env HARNESS_TIMEOUT_GRACE="$bad" "$WRAPPER" 5 label DEMO_TIMEOUT true
  [ "$STATUS" -eq 2 ] || fail "grace '$bad' exited $STATUS, expected 2"
  expect_output 'HARNESS_TIMEOUT_GRACE must be a whole number of seconds greater than 0'
done

# --- bootstrap --------------------------------------------------------------

# A fake npm that hangs on `npm ci` and `npm test`, and passes `npm run lint`.
FAKE_BIN="$TMP_ROOT/fake-bin"
mkdir -p "$FAKE_BIN"
cat > "$FAKE_BIN/npm" <<EOF
#!/bin/sh
case "\$1" in
  ci|test) exec "$HANG" ;;
esac
exit 0
EOF
chmod +x "$FAKE_BIN/npm"
FAKE_PATH="$FAKE_BIN:$PATH"

PROJECT="$TMP_ROOT/projects/node"
mkdir -p "$PROJECT"
printf '%s\n' '{"name":"fixture","version":"1.0.0","scripts":{"lint":"true","test":"true"}}' > "$PROJECT/package.json"
printf '%s\n' '{}' > "$PROJECT/package-lock.json"

timed env PATH="$FAKE_PATH" HARNESS_BOOTSTRAP_TIMEOUT=1 "$INIT" --project "$PROJECT" --yes
[ "$STATUS" -eq 1 ] || fail "a hung bootstrap command exited $STATUS, expected 1"
[ "$ELAPSED" -le 8 ] || fail "the bootstrap timeout took ${ELAPSED}s for a 1s limit"
expect_output 'TIMEOUT: installing JavaScript/TypeScript dependencies with npm ci did not finish within 1s'
expect_output 'Set HARNESS_BOOTSTRAP_TIMEOUT to allow more time'
expect_output 'FAIL: installing JavaScript/TypeScript dependencies with npm ci (exit 124)'
expect_group_stopped

# Automatic mode records the timeout as a failed bootstrap with exit 124.
timed env PATH="$FAKE_PATH" HARNESS_BOOTSTRAP_TIMEOUT=1 HARNESS_AUTO_INIT_SYNC=1 "$INIT" --project "$PROJECT" --auto
[ "$STATUS" -eq 0 ] || fail "init --auto exited $STATUS, expected 0"
expect_output 'bootstrap FAILED (exit 124)'
STATE=$(find "$HARNESS_DB_ROOT/targets" -name bootstrap.state -exec grep -l "PROJECT_ROOT=$PROJECT\$" {} \;)
[ -n "$STATE" ] || fail "automatic mode wrote no bootstrap state"
grep -q '^STATUS=failed$' "$STATE" || fail "a timed-out automatic bootstrap was not recorded as failed"
grep -q '^EXIT=124$' "$STATE" || fail "a timed-out automatic bootstrap did not record exit 124"
grep -q 'TIMEOUT: installing JavaScript/TypeScript dependencies with npm ci' "${STATE%.state}.log" || fail "the bootstrap log does not explain the timeout"
expect_group_stopped

timed env HARNESS_BOOTSTRAP_TIMEOUT=soon "$INIT" --project "$PROJECT" --yes
[ "$STATUS" -eq 2 ] || fail "an invalid HARNESS_BOOTSTRAP_TIMEOUT exited $STATUS, expected 2"
expect_output 'HARNESS_BOOTSTRAP_TIMEOUT must be a whole number of seconds greater than 0'

# --- verification -----------------------------------------------------------

# The hung test fails on its own; lint still runs and passes.
timed env PATH="$FAKE_PATH" HARNESS_VERIFY_TIMEOUT=1 "$VERIFY" --project "$PROJECT"
[ "$STATUS" -eq 1 ] || fail "verify with a hung test exited $STATUS, expected 1"
[ "$ELAPSED" -le 8 ] || fail "the verification timeout took ${ELAPSED}s for a 1s limit"
expect_output 'TIMEOUT: node:test did not finish within 1s'
expect_output 'Set HARNESS_VERIFY_TIMEOUT to allow more time'
grep -q -x -F 'FAIL: node:test (exit 124)' "$OUT" || fail "the hung test was not reported as failed with exit 124"
grep -q -x -F 'PASS: node:lint' "$OUT" || fail "lint did not run next to the hung test"
RECORD=$(sed -n 's/^Run record: //p' "$OUT")
[ -n "$RECORD" ] || fail "verify wrote no run record"
grep -q '^EXIT=1$' "$RECORD" || fail "the run record does not show the failure"
expect_group_stopped

# A hung harness test file is named, and the files after it still run.
HARNESS_PROJECT="$TMP_ROOT/projects/harness-shaped"
mkdir -p "$HARNESS_PROJECT/scripts" "$HARNESS_PROJECT/tests"
printf '%s\n' '#!/usr/bin/env sh' 'exit 0' > "$HARNESS_PROJECT/scripts/harness"
chmod +x "$HARNESS_PROJECT/scripts/harness"
printf '%s\n' '#!/usr/bin/env sh' "exec '$HANG'" > "$HARNESS_PROJECT/tests/a-hangs.sh"
printf '%s\n' '#!/usr/bin/env sh' "touch '$TMP_ROOT/later-test-ran'" > "$HARNESS_PROJECT/tests/b-later.sh"
timed env HARNESS_VERIFY_TIMEOUT=1 "$VERIFY" --project "$HARNESS_PROJECT"
[ "$STATUS" -eq 1 ] || fail "verify with a hung harness test exited $STATUS, expected 1"
expect_output 'TIMEOUT: tests/a-hangs.sh did not finish within 1s'
grep -q '^FAIL: harness:tests (exit 1)$' "$OUT" || fail "harness:tests did not fail"
[ -f "$TMP_ROOT/later-test-ran" ] || fail "the test file after the hung one did not run"
expect_group_stopped

timed env HARNESS_VERIFY_TIMEOUT=0 "$VERIFY" --project "$PROJECT"
[ "$STATUS" -eq 2 ] || fail "an invalid HARNESS_VERIFY_TIMEOUT exited $STATUS, expected 2"
expect_output 'HARNESS_VERIFY_TIMEOUT must be a whole number of seconds greater than 0'

# --- no supervisor: refuse before any project code runs ----------------------

# A Makefile whose target discovery itself runs project code: `make -qp`
# expands $(shell ...). The markers show whether the probe or a command ran.
MARK_BIN="$TMP_ROOT/mark-bin"
mkdir -p "$MARK_BIN"
printf '#!/bin/sh\ntouch "%s"\n' "$TMP_ROOT/npm-ran" > "$MARK_BIN/npm"
chmod +x "$MARK_BIN/npm"
UNSUPERVISED="$TMP_ROOT/projects/unsupervised"
mkdir -p "$UNSUPERVISED"
printf '%s\n' '{"name":"fixture","version":"1.0.0"}' > "$UNSUPERVISED/package.json"
printf '%s\n' '{}' > "$UNSUPERVISED/package-lock.json"
if command -v make >/dev/null 2>&1; then
  printf 'PROBE := %s(shell touch "%s")\ninit:\n\t@touch "%s"\n' '$' "$TMP_ROOT/probe-ran" "$TMP_ROOT/init-ran" > "$UNSUPERVISED/Makefile"
fi

timed env PATH="$MARK_BIN:$NO_TOOLS" "$INIT" --project "$UNSUPERVISED" --yes
[ "$STATUS" -eq 1 ] || fail "init without a supervisor exited $STATUS, expected refusal exit 1"
expect_output 'REFUSED: no timeout supervisor'
expect_output 'Nothing was run'
[ ! -e "$TMP_ROOT/npm-ran" ] || fail "npm ran without a supervisor"
[ ! -e "$TMP_ROOT/probe-ran" ] || fail "make -qp ran the Makefile without a supervisor"
[ ! -e "$TMP_ROOT/init-ran" ] || fail "make init ran without a supervisor"

timed env PATH="$MARK_BIN:$NO_TOOLS" HARNESS_AUTO_INIT_SYNC=1 "$INIT" --project "$UNSUPERVISED" --auto
expect_output 'bootstrap REFUSED, nothing was run: no timeout supervisor'
STATE=$(find "$HARNESS_DB_ROOT/targets" -name bootstrap.state -exec grep -l "PROJECT_ROOT=$UNSUPERVISED\$" {} \;)
grep -q '^STATUS=failed$' "$STATE" || fail "an unsupervised automatic bootstrap was not recorded as failed"
if [ -e "$TMP_ROOT/npm-ran" ] || [ -e "$TMP_ROOT/probe-ran" ]; then
  fail "automatic mode ran project code without a supervisor"
fi

# Without a Makefile, the queued install alone is refused.
UNSUPERVISED_NODE="$TMP_ROOT/projects/unsupervised-node"
mkdir -p "$UNSUPERVISED_NODE"
cp "$UNSUPERVISED/package.json" "$UNSUPERVISED/package-lock.json" "$UNSUPERVISED_NODE/"
timed env PATH="$MARK_BIN:$NO_TOOLS" "$INIT" --project "$UNSUPERVISED_NODE" --yes
[ "$STATUS" -eq 1 ] || fail "init of a Node project without a supervisor exited $STATUS, expected refusal exit 1"
expect_output 'REFUSED: no timeout supervisor'
[ ! -e "$TMP_ROOT/npm-ran" ] || fail "npm ci ran without a supervisor"

# Nothing to run needs no supervisor.
EMPTY="$TMP_ROOT/projects/empty"
mkdir -p "$EMPTY"
timed env PATH="$NO_TOOLS" "$INIT" --project "$EMPTY" --yes
[ "$STATUS" -eq 0 ] || fail "init of an empty project without a supervisor exited $STATUS, expected 0"
expect_output 'No project-owned setup commands detected'

timed env PATH="$MARK_BIN:$NO_TOOLS" "$VERIFY" --project "$UNSUPERVISED"
[ "$STATUS" -eq 2 ] || fail "verify without a supervisor exited $STATUS, expected 2"
expect_output 'FAIL: no timeout supervisor'
[ ! -e "$TMP_ROOT/probe-ran" ] || fail "verify ran make -qp without a supervisor"
[ ! -e "$TMP_ROOT/npm-ran" ] || fail "verify ran npm without a supervisor"

# --- hanging discovery -------------------------------------------------------

if command -v make >/dev/null 2>&1; then
  MAKE_HANG="$TMP_ROOT/projects/make-hang"
  mkdir -p "$MAKE_HANG/scripts"
  printf 'PROBE := %s(shell "%s")\ninit:\n\t@touch "%s"\ntest:\n\t@true\n' '$' "$HANG" "$TMP_ROOT/init-ran" > "$MAKE_HANG/Makefile"
  printf '%s\n' '#!/usr/bin/env sh' 'exit 0' > "$MAKE_HANG/scripts/ok.sh"

  timed env HARNESS_BOOTSTRAP_TIMEOUT=1 "$INIT" --project "$MAKE_HANG" --yes
  [ "$STATUS" -eq 1 ] || fail "init with a hanging make -qp exited $STATUS, expected refusal exit 1"
  [ "$ELAPSED" -le 8 ] || fail "the make discovery timeout took ${ELAPSED}s for a 1s limit"
  expect_output 'REFUSED: discovering Makefile targets with make -qp did not finish within 1s'
  expect_output 'HARNESS_BOOTSTRAP_TIMEOUT'
  [ ! -e "$TMP_ROOT/init-ran" ] || fail "make init ran after its discovery timed out"
  expect_group_stopped

  timed env HARNESS_BOOTSTRAP_TIMEOUT=1 HARNESS_AUTO_INIT_SYNC=1 "$INIT" --project "$MAKE_HANG" --auto
  expect_output 'bootstrap REFUSED, nothing was run: discovering Makefile targets'
  STATE=$(find "$HARNESS_DB_ROOT/targets" -name bootstrap.state -exec grep -l "PROJECT_ROOT=$MAKE_HANG\$" {} \;)
  grep -q '^STATUS=failed$' "$STATE" || fail "a hung automatic discovery was not recorded as failed"
  expect_group_stopped

  # Verification fails once for the probe, and the shell checks still run.
  timed env HARNESS_VERIFY_TIMEOUT=1 "$VERIFY" --project "$MAKE_HANG"
  [ "$STATUS" -eq 1 ] || fail "verify with a hanging make -qp exited $STATUS, expected 1"
  [ "$ELAPSED" -le 8 ] || fail "verify's make discovery took ${ELAPSED}s for a 1s limit"
  expect_output 'TIMEOUT: make:discover did not finish within 1s'
  grep -q -x -F 'FAIL: make:discover (exit 124)' "$OUT" || fail "the hung make discovery was not reported"
  [ "$(grep -c 'make:discover' "$OUT")" -eq 2 ] || fail "make -qp ran more than once"
  grep -q -x -F 'PASS: bash:syntax' "$OUT" || fail "the shell checks did not run after the hung discovery"
  expect_group_stopped
fi

# A package.json script listing that hangs (here a hanging node) fails too.
NODE_BIN="$TMP_ROOT/node-bin"
mkdir -p "$NODE_BIN"
ln -s "$HANG" "$NODE_BIN/node"
NODE_HANG="$TMP_ROOT/projects/node-hang"
mkdir -p "$NODE_HANG"
printf '%s\n' '{"name":"fixture","version":"1.0.0","scripts":{"test":"true"}}' > "$NODE_HANG/package.json"
timed env PATH="$NODE_BIN:$FAKE_PATH" HARNESS_VERIFY_TIMEOUT=1 "$VERIFY" --project "$NODE_HANG"
[ "$STATUS" -eq 1 ] || fail "verify with a hanging package.json discovery exited $STATUS, expected 1"
expect_output 'TIMEOUT: package.json:discover did not finish within 1s'
grep -q -x -F 'FAIL: package.json:discover (exit 124)' "$OUT" || fail "the hung package.json discovery was not reported"
expect_group_stopped

printf '%s\n' 'PASS: bootstrap and verification commands stop at their time limit'
