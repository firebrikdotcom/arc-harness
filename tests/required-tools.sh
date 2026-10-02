#!/usr/bin/env sh
set -eu

# One list of required tools (.harness-required-tools) drives both the local
# bootstrap check and the CI install, so local and CI verification cannot
# silently run different tool sets.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
TOOLS="$ROOT/scripts/required-tools.sh"
INIT="$ROOT/scripts/init.sh"
SH=$(command -v sh)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-required-tools.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
OUT="$TMP_ROOT/out.txt"
# init.sh registers targets; keep that out of the machine registry.
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

# run_status CMD...  Runs CMD with output in $OUT and prints its exit status.
run_status() {
  _status=0
  "$@" > "$OUT" 2>&1 < /dev/null || _status=$?
  printf '%s\n' "$_status"
}

[ -x "$TOOLS" ] || fail "scripts/required-tools.sh is missing or not executable"

absent=harness-absent-tool-38
present=harness-present-tool-38
bin="$TMP_ROOT/bin"
mkdir -p "$bin"
printf '#!/bin/sh\nexit 0\n' > "$bin/$present"
chmod 755 "$bin/$present"
PATH="$bin:$PATH"
export PATH

# --- the checker --------------------------------------------------------------

project="$TMP_ROOT/listed"
mkdir -p "$project"
printf '%s\n' \
  '# comment line' \
  '' \
  "$present	-    # tab-separated, provided by the environment" \
  "$absent harness-absent-pkg" \
  "$present duplicate-ignored" > "$project/.harness-required-tools"
printf 'last-tool last-pkg' >> "$project/.harness-required-tools"

[ "$(run_status "$TOOLS" check --project "$project")" -eq 1 ] || fail "a missing listed tool did not exit 1"
expect_output "found: $present"
expect_output "missing required tool: $absent (apt package: harness-absent-pkg)"
expect_output 'missing required tool: last-tool (apt package: last-pkg)'
[ "$(grep -c "$present" "$OUT")" -eq 1 ] || fail "a duplicated tool was reported twice"

[ "$(run_status "$TOOLS" check --quiet --project "$project")" -eq 1 ] || fail "check --quiet did not exit 1"
! grep -q '^found:' "$OUT" || fail "check --quiet printed found tools"
expect_output "missing required tool: $absent"

[ "$(run_status "$TOOLS" packages --project "$project")" -eq 0 ] || fail "packages failed on a valid list"
[ "$(tr '\n' ' ' < "$OUT")" = "harness-absent-pkg duplicate-ignored last-pkg " ] \
  || fail "packages should list each package once, in order, skipping '-': $(tr '\n' ' ' < "$OUT")"

# git and sh are required even without a list, and the checker needs no
# external command, so it still reports them on a PATH that lacks them.
bare="$TMP_ROOT/bare"
mkdir -p "$bare" "$TMP_ROOT/empty-bin"
[ "$(run_status env PATH="$TMP_ROOT/empty-bin" "$SH" "$TOOLS" check --project "$bare")" -eq 1 ] \
  || fail "missing git and sh did not exit 1"
expect_output 'missing required tool: git'
expect_output 'missing required tool: sh'
[ "$(run_status "$TOOLS" packages --project "$bare")" -eq 0 ] || fail "packages failed without a list"
[ ! -s "$OUT" ] || fail "packages printed something without a list"

# A malformed list is an error (exit 2), never a partial pass, and a name can
# not smuggle shell syntax or an apt-get option into CI.
malformed() {
  label=$1
  shift
  mkdir -p "$TMP_ROOT/bad"
  printf '%s\n' "$@" > "$TMP_ROOT/bad/.harness-required-tools"
  [ "$(run_status "$TOOLS" check --project "$TMP_ROOT/bad")" -eq 2 ] || fail "$label: check did not exit 2"
  [ "$(run_status "$TOOLS" packages --project "$TMP_ROOT/bad")" -eq 2 ] || fail "$label: packages did not exit 2"
}
malformed 'three fields' 'git git extra'
malformed 'command substitution in a tool' "\$(touch $TMP_ROOT/INJECTED)"
malformed 'an option as a package' 'shellcheck --allow-downgrades'
malformed 'an option as a tool' '-y'
malformed 'a glob' 'shell*'
[ ! -e "$TMP_ROOT/INJECTED" ] || fail "a tool name was executed"
expect_output 'invalid tool name'

# --- scripts/init.sh uses the list -------------------------------------------

if command -v make >/dev/null 2>&1; then
  target="$TMP_ROOT/target"
  mkdir -p "$target"
  printf 'init:\n\t@touch RAN_PROJECT_SETUP\n' > "$target/Makefile"
  printf '%s\n' "$absent harness-absent-pkg" > "$target/.harness-required-tools"

  [ "$(run_status "$INIT" --project "$target" --yes)" -eq 1 ] || fail "init --yes with a missing tool did not exit 1"
  expect_output "missing required tool: $absent"
  expect_output 'Install missing required tools'
  [ ! -f "$target/RAN_PROJECT_SETUP" ] || fail "init ran project setup despite a missing tool"

  # Session start: the reason reaches the hook's context line, nothing runs.
  [ "$(run_status env HARNESS_AUTO_INIT_SYNC=1 "$INIT" --project "$target" --auto)" -eq 1 ] \
    || fail "init --auto with a missing tool did not exit 1"
  expect_output "^Harness auto-init: missing required tool: $absent"
  expect_output '^Harness auto-init: bootstrap not run'
  [ ! -f "$target/RAN_PROJECT_SETUP" ] || fail "init --auto ran project setup despite a missing tool"

  printf '%s\n' 'git git git' > "$target/.harness-required-tools"
  [ "$(run_status "$INIT" --project "$target" --yes)" -eq 2 ] || fail "init with a malformed list did not exit 2"
  [ ! -f "$target/RAN_PROJECT_SETUP" ] || fail "init ran project setup with a malformed list"

  printf '%s\n' "$present" > "$target/.harness-required-tools"
  [ "$(run_status "$INIT" --project "$target" --yes)" -eq 0 ] || fail "init with every listed tool present failed"
  expect_output "found: $present"
  [ -f "$target/RAN_PROJECT_SETUP" ] || fail "init did not run project setup once the tools were present"
else
  printf '%s\n' 'SKIP: make is unavailable; init integration cases need a Makefile target'
fi

# --- the harness's own list and CI -------------------------------------------

[ -f "$ROOT/.harness-required-tools" ] || fail "the harness has no .harness-required-tools"
[ "$(run_status "$TOOLS" packages --project "$ROOT")" -eq 0 ] || fail "the harness list is malformed"
listed=$(sed 's/#.*//' "$ROOT/.harness-required-tools" | awk 'NF { print $1 }')

# Every tool a harness test gates on must be listed, or verification would
# skip coverage locally that CI runs. Package managers are the target
# ecosystem's business, not the harness's, so they stay optional; GNU
# `timeout`/`gtimeout` are only the fallback time-limit supervisor behind
# python3, which is listed.
optional=' npm pnpm yarn timeout gtimeout '
gated=$(
  for file in "$ROOT"/tests/*.sh; do
    [ "$file" = "$ROOT/tests/required-tools.sh" ] && continue
    grep -o 'command -v [A-Za-z0-9_.+-][A-Za-z0-9_.+-]*' "$file" | awk '{ print $3 }'
    # Only literal names: a loop over "$dir"/* or a variable is not a tool gate.
    sed -n 's/^[[:space:]]*for tool in \([^;]*\);.*/\1/p' "$file" | tr ' ' '\n' \
      | grep -E '^[A-Za-z0-9_.+-]+$' || :
  done | awk 'NF' | sort -u
)
[ -n "$gated" ] || fail "found no tool gates in tests/*.sh; the scan is broken"
for tool in $gated; do
  case "$optional" in *" $tool "*) continue ;; esac
  printf '%s\n' "$listed" | grep -qx -- "$tool" || fail "tests gate on '$tool' but .harness-required-tools does not list it"
done

# CI installs exactly the list: one step reads `required-tools.sh packages`, it
# runs before the bootstrap check, and nothing else in a workflow installs a
# system tool.
workflow="$ROOT/.github/workflows/ci.yml"
[ -f "$workflow" ] || fail "no CI workflow"
[ "$(grep -c 'scripts/required-tools.sh packages' "$workflow")" -eq 1 ] \
  || fail "ci.yml must read the package list from scripts/required-tools.sh packages exactly once"
installs=$(grep -hnE '(apt-get|apt|brew|snap|pipx?|pip3|gem|choco)[[:space:]]+install([[:space:]]|$)' "$ROOT"/.github/workflows/*.yml "$ROOT"/.github/workflows/*.yaml 2>/dev/null || :)
[ "$(printf '%s\n' "$installs" | awk 'NF' | wc -l | tr -d ' ')" -eq 1 ] \
  || fail "workflows must install system tools only from the list; found: $installs"
# shellcheck disable=SC2016 # $packages is the literal workflow text to match
printf '%s\n' "$installs" | grep -q 'apt-get install --yes --no-upgrade \$packages$' \
  || fail "the CI install must be 'apt-get install --yes --no-upgrade \$packages'; found: $installs"
install_line=$(grep -n 'scripts/required-tools.sh packages' "$workflow" | cut -d: -f1)
init_line=$(grep -n 'scripts/init.sh --yes' "$workflow" | head -n 1 | cut -d: -f1)
[ -n "$init_line" ] || fail "ci.yml does not run scripts/init.sh --yes"
[ "$install_line" -lt "$init_line" ] || fail "ci.yml must install the required tools before scripts/init.sh --yes checks them"

printf '%s\n' 'PASS: one required-tools list drives the init check and the CI install'
