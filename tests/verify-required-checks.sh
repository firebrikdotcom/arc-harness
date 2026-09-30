#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
VERIFY="$HARNESS_ROOT/scripts/verify.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-verify.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# Keep fixture run records out of the real harness database.
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
# Each fixture declares its own requirements; a caller's override must not leak in.
unset HARNESS_REQUIRED_CHECKS

fail() {
  printf 'FAIL: %s\n' "$1"
  if [ -f "$TMP_ROOT/output" ]; then
    cat "$TMP_ROOT/output"
  fi
  exit 1
}

# Line number of the first output line equal to $1, or empty.
line_of() {
  grep -n -x -F "$1" "$TMP_ROOT/output" | head -n 1 | cut -d: -f1
}

lint_project="$TMP_ROOT/lint"
test_project="$TMP_ROOT/shell-only-test"
real_test_project="$TMP_ROOT/real-test"
missing_project="$TMP_ROOT/missing"
override_project="$TMP_ROOT/override"
make_lint_project="$TMP_ROOT/make-lint"
mkdir -p "$lint_project/scripts" "$test_project/scripts" "$real_test_project" "$missing_project" "$override_project/scripts"
for project in "$lint_project" "$test_project" "$override_project"; do
  printf '%s\n' '#!/usr/bin/env sh' 'exit 0' > "$project/scripts/check.sh"
done
printf '%s\n' 'lint' > "$lint_project/.harness-required-checks"
printf '%s\n' 'test' > "$test_project/.harness-required-checks"
printf '%s\n' 'lint' > "$missing_project/.harness-required-checks"
printf '%s\n' 'test' > "$override_project/.harness-required-checks"

# The shell syntax check belongs to lint: it satisfies a required lint
# category and reports before lint's verdict line.
"$VERIFY" --project "$lint_project" > "$TMP_ROOT/output" 2>&1 || fail 'shell syntax check did not satisfy required lint'
syntax_line=$(line_of 'PASS: bash:syntax')
lint_line=$(line_of 'REQUIRED: lint satisfied')
[ -n "$syntax_line" ] || fail 'bash:syntax did not run for a shell project'
[ -n "$lint_line" ] || fail 'required lint was not reported satisfied'
[ "$syntax_line" -lt "$lint_line" ] || fail 'bash:syntax ran outside the lint category'

# A syntax check is not a test, so shell files alone cannot satisfy test.
if "$VERIFY" --project "$test_project" > "$TMP_ROOT/output" 2>&1; then
  fail 'shell syntax check satisfied required test'
fi
grep -q -x -F "FAIL: required category 'test' ran no checks" "$TMP_ROOT/output" || fail 'required test failure was not reported'

# A real test runner still satisfies test.
if command -v make >/dev/null 2>&1; then
  printf 'test:\n\t@true\n' > "$real_test_project/Makefile"
  printf '%s\n' 'test' > "$real_test_project/.harness-required-checks"
  "$VERIFY" --project "$real_test_project" > "$TMP_ROOT/output" 2>&1 || fail 'make test did not satisfy required test'
  grep -q -x -F 'PASS: make:test' "$TMP_ROOT/output" || fail 'make:test did not run'
  grep -q -x -F 'REQUIRED: test satisfied' "$TMP_ROOT/output" || fail 'required test was not reported satisfied'

  # A Makefile lint target replaces generic lint detection, but not the shell
  # syntax check: a script that does not parse still fails verification.
  mkdir -p "$make_lint_project/scripts"
  printf 'lint:\n\t@true\n' > "$make_lint_project/Makefile"
  printf '%s\n' '#!/usr/bin/env sh' 'if then' > "$make_lint_project/scripts/broken.sh"
  if "$VERIFY" --project "$make_lint_project" > "$TMP_ROOT/output" 2>&1; then
    fail 'an unparseable script passed behind a make lint target'
  fi
  grep -q '^FAIL: bash:syntax (exit [0-9]*)$' "$TMP_ROOT/output" || fail 'bash:syntax did not run behind a make lint target'
  grep -q -x -F 'PASS: make:lint' "$TMP_ROOT/output" || fail 'make:lint did not run'
fi

if "$VERIFY" --project "$missing_project" >/dev/null 2>&1; then
  fail 'missing required category unexpectedly passed'
fi

# Verifying a harness-shaped target under the override still enforces it on
# that target, but its regression scripts do not inherit it.
harness_project="$TMP_ROOT/harness-shaped"
probe_out="$TMP_ROOT/probe-saw"
mkdir -p "$harness_project/scripts" "$harness_project/tests"
printf '%s\n' '#!/usr/bin/env sh' 'exit 0' > "$harness_project/scripts/harness"
chmod +x "$harness_project/scripts/harness"
printf '%s\n' '#!/usr/bin/env sh' "printf '%s\\n' \"\${HARNESS_REQUIRED_CHECKS-unset}\" > '$probe_out'" > "$harness_project/tests/probe.sh"
HARNESS_REQUIRED_CHECKS='lint test' "$VERIFY" --project "$harness_project" > "$TMP_ROOT/output" 2>&1 || fail 'harness-shaped target failed under HARNESS_REQUIRED_CHECKS'
grep -q -x -F 'Required checks (environment): lint test' "$TMP_ROOT/output" || fail 'outer override was not applied to the target'
grep -q -x -F 'REQUIRED: lint satisfied' "$TMP_ROOT/output" || fail 'outer lint requirement was not enforced'
grep -q -x -F 'REQUIRED: test satisfied' "$TMP_ROOT/output" || fail 'outer test requirement was not enforced'
[ "$(cat "$probe_out")" = 'unset' ] || fail "regression script inherited HARNESS_REQUIRED_CHECKS=$(cat "$probe_out")"

# The environment overrides a file that would otherwise fail.
HARNESS_REQUIRED_CHECKS='lint' "$VERIFY" --project "$override_project" > "$TMP_ROOT/output" 2>&1 || fail 'HARNESS_REQUIRED_CHECKS did not override the file'

printf '%s\n' 'PASS: required-check enforcement'
