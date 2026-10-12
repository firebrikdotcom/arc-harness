#!/usr/bin/env sh
# Bootstrap installs with the package manager that owns the lockfile, or
# refuses and runs nothing. Real package managers are hidden from PATH and
# replaced by fakes that only record how they were called.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
INIT="$HARNESS_ROOT_UNDER_TEST/scripts/init.sh"
TARGET_HELPER="$HARNESS_ROOT_UNDER_TEST/scripts/harness-target.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-init-pm.XXXXXX")
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
trap 'rm -rf "${TMP_ROOT:?}"' EXIT HUP INT TERM
OUT="$TMP_ROOT/out.txt"
CALLS="$TMP_ROOT/calls.txt"
# Registration must land in an isolated database, never the machine registry.
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT

PACKAGE_MANAGERS="npm npx pnpm pnpx yarn yarnpkg bun bunx corepack"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  [ -f "$CALLS" ] && { printf '%s\n' "--- package manager calls ---"; cat "$CALLS"; }
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

# Every tool on PATH except the package managers, so a missing manager is
# really missing even on a machine that has it installed.
SYSTEM_BIN="$TMP_ROOT/system-bin"
mkdir -p "$SYSTEM_BIN"
_old_ifs=$IFS
IFS=:
for dir in $PATH; do
  [ -d "$dir" ] || continue
  for tool in "$dir"/*; do
    if [ ! -f "$tool" ] || [ ! -x "$tool" ]; then
      continue
    fi
    name=${tool##*/}
    case " $PACKAGE_MANAGERS " in
      *" $name "*) continue ;;
    esac
    [ -e "$SYSTEM_BIN/$name" ] || ln -s "$tool" "$SYSTEM_BIN/$name"
  done
done
IFS=$_old_ifs

FAKE_BIN="$TMP_ROOT/fake-bin"
mkdir -p "$FAKE_BIN"
for pm in npm pnpm yarn bun; do
  printf '#!/bin/sh\nprintf "%%s\\n" "%s $*" >> "%s"\n' "$pm" "$CALLS" > "$FAKE_BIN/$pm"
  chmod +x "$FAKE_BIN/$pm"
done

# with_managers "pm ..."  Builds a PATH holding only the named fake managers.
with_managers() {
  _bin="$TMP_ROOT/bin-$(printf '%s' "$1" | tr ' ' '-')"
  if [ ! -d "$_bin" ]; then
    mkdir -p "$_bin"
    for pm in $1; do
      ln -s "$FAKE_BIN/$pm" "$_bin/$pm"
    done
  fi
  printf '%s:%s\n' "$_bin" "$SYSTEM_BIN"
}

# new_project NAME FILE...  A project with package.json and the given lockfiles.
new_project() {
  _dir="$TMP_ROOT/projects/$1"
  shift
  mkdir -p "$_dir"
  printf '{"name":"fixture","version":"1.0.0"}\n' > "$_dir/package.json"
  for _file in "$@"; do
    printf 'fixture lockfile\n' > "$_dir/$_file"
  done
  printf '%s\n' "$_dir"
}

# run_init "MANAGERS" PROJECT ARGS...  Runs init; sets STATUS.
run_init() {
  _path=$(with_managers "$1")
  _project=$2
  shift 2
  : > "$CALLS"
  STATUS=0
  PATH="$_path" "$INIT" --project "$_project" "$@" > "$OUT" 2>&1 < /dev/null || STATUS=$?
}

# expect_install "MANAGERS" PROJECT "CALL"  Exactly this one install ran.
expect_install() {
  run_init "$1" "$2" --yes
  [ "$STATUS" -eq 0 ] || fail "init exited $STATUS for $2, expected 0"
  [ "$(cat "$CALLS")" = "$3" ] || fail "expected only '$3' to run for $2"
}

# expect_refusal "MANAGERS" PROJECT PATTERN...  Nothing ran; exit 1; message matches.
expect_refusal() {
  _managers=$1
  _project=$2
  shift 2
  run_init "$_managers" "$_project" --yes
  [ "$STATUS" -eq 1 ] || fail "init exited $STATUS for $_project, expected refusal exit 1"
  [ ! -s "$CALLS" ] || fail "a package manager ran although the bootstrap refused"
  expect_output 'REFUSED:'
  expect_output 'Nothing was run'
  for _pattern in "$@"; do
    expect_output "$_pattern"
  done
}

ALL="npm pnpm yarn bun"

# Each lockfile installs only with its owner, frozen, even when others exist.
expect_install "$ALL" "$(new_project yarn yarn.lock)" "yarn install --frozen-lockfile"
expect_install "$ALL" "$(new_project pnpm pnpm-lock.yaml)" "pnpm install --frozen-lockfile"
expect_install "$ALL" "$(new_project npm package-lock.json)" "npm ci"
expect_install "$ALL" "$(new_project shrinkwrap npm-shrinkwrap.json)" "npm ci"
expect_install "$ALL" "$(new_project bun bun.lock)" "bun install --frozen-lockfile"
expect_install "$ALL" "$(new_project bunb bun.lockb)" "bun install --frozen-lockfile"
# Two lockfiles from the same manager are not a conflict.
expect_install "$ALL" "$(new_project npm-both package-lock.json npm-shrinkwrap.json)" "npm ci"

# The lockfile's manager is missing: refuse instead of using another one.
YARN_PROJECT=$(new_project yarn-missing yarn.lock)
expect_refusal "npm pnpm bun" "$YARN_PROJECT" 'yarn\.lock was written by yarn, but yarn is not installed'
expect_refusal "npm yarn" "$(new_project pnpm-missing pnpm-lock.yaml)" 'pnpm-lock\.yaml was written by pnpm'
expect_refusal "pnpm yarn" "$(new_project npm-missing package-lock.json)" 'package-lock\.json was written by npm'
expect_refusal "npm" "$(new_project bun-missing bun.lockb)" 'bun\.lockb was written by bun'

# Lockfiles from different managers: the bootstrap cannot know which was tested.
expect_refusal "$ALL" "$(new_project conflict yarn.lock package-lock.json)" \
  'different package managers' 'yarn\.lock, package-lock\.json'

# A refusal needs no confirmation and runs no other project command either.
if command -v make >/dev/null 2>&1; then
  printf 'init:\n\t@touch RAN_PROJECT_SETUP\n' > "$YARN_PROJECT/Makefile"
  expect_refusal "npm" "$YARN_PROJECT" 'yarn is not installed'
  [ ! -f "$YARN_PROJECT/RAN_PROJECT_SETUP" ] || fail "make init ran although the bootstrap refused"
  rm -f "$YARN_PROJECT/Makefile"
fi
run_init "npm" "$YARN_PROJECT"
[ "$STATUS" -eq 1 ] || fail "refusal without --yes exited $STATUS, expected 1"
expect_output 'REFUSED:'
[ ! -s "$CALLS" ] || fail "a package manager ran without confirmation"

# Without a lockfile the first available manager is used, as before.
UNLOCKED=$(new_project unlocked)
expect_install "$ALL" "$UNLOCKED" "pnpm install"
expect_install "npm yarn" "$UNLOCKED" "yarn install"
expect_install "npm" "$UNLOCKED" "npm install"
run_init "" "$UNLOCKED" --yes
[ "$STATUS" -eq 0 ] || fail "init without any package manager exited $STATUS, expected 0"
expect_output 'npm/pnpm/yarn is unavailable'

# A stray lockfile without package.json is not a Node project.
STRAY="$TMP_ROOT/projects/stray"
mkdir -p "$STRAY"
printf 'fixture lockfile\n' > "$STRAY/yarn.lock"
run_init "npm" "$STRAY" --yes
[ "$STATUS" -eq 0 ] || fail "init with a lockfile but no package.json exited $STATUS, expected 0"
expect_output 'No package.json found'
[ ! -s "$CALLS" ] || fail "a package manager ran without package.json"

# Automatic mode records the refusal instead of installing with another manager.
AUTO_PROJECT=$(new_project auto yarn.lock)
run_init "npm pnpm" "$AUTO_PROJECT" --auto
[ "$STATUS" -eq 0 ] || fail "init --auto exited $STATUS, expected 0"
[ ! -s "$CALLS" ] || fail "a package manager ran in automatic mode although the bootstrap refused"
expect_output 'bootstrap REFUSED, nothing was run'
expect_output 'yarn is not installed'
STATE=$(find "$HARNESS_DB_ROOT/targets" -name bootstrap.state -exec grep -l "PROJECT_ROOT=$AUTO_PROJECT\$" {} \;)
[ -n "$STATE" ] || fail "automatic mode wrote no bootstrap state"
grep -q '^STATUS=failed$' "$STATE" || fail "automatic refusal was not recorded as failed"
grep -q '^EXIT=1$' "$STATE" || fail "automatic refusal did not record exit 1"
grep -q 'REFUSED: yarn\.lock was written by yarn' "${STATE%.state}.log" || fail "bootstrap log does not explain the refusal"
run_init "npm pnpm" "$AUTO_PROJECT" --auto
expect_output 'bootstrap failed earlier (exit 1)'
[ ! -s "$CALLS" ] || fail "a package manager ran on the next automatic start"

# Once the owning manager is installed, a rerun installs with it.
run_init "npm pnpm yarn" "$AUTO_PROJECT" --yes
[ "$(cat "$CALLS")" = "yarn install --frozen-lockfile" ] || fail "yarn did not install once it was available"

# Newly recognised lockfiles change the bootstrap fingerprint.
FP_PROJECT=$(new_project fingerprint)
before=$("$TARGET_HELPER" fingerprint "$FP_PROJECT")
for lock in npm-shrinkwrap.json bun.lock bun.lockb; do
  printf 'fixture lockfile\n' > "$FP_PROJECT/$lock"
  after=$("$TARGET_HELPER" fingerprint "$FP_PROJECT")
  [ "$after" != "$before" ] || fail "$lock does not change the bootstrap fingerprint"
  before=$after
done

printf '%s\n' 'PASS: bootstrap installs with the lockfile owner or refuses and runs nothing'
