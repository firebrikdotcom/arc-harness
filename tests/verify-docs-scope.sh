#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
VERIFY="$HARNESS_ROOT/scripts/verify.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-docs-scope.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# Keep fixture run records out of the real harness database.
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_VERIFY_SCOPE HARNESS_REQUIRED_CHECKS || :
# Fixture runs must not record checks against the calling agent session.
HARNESS_SESSION_ID='' CODEX_THREAD_ID='' CLAUDE_SESSION_ID='' HARNESS_JEV_CHECKPOINTS=0
export HARNESS_SESSION_ID CODEX_THREAD_ID CLAUDE_SESSION_ID HARNESS_JEV_CHECKPOINTS

fail() {
  printf 'FAIL: %s\n' "$1"
  exit 1
}

# A project whose lint passes and whose test target always fails, branched
# from `main` with `main` as its upstream.
new_project() {
  project="$TMP_ROOT/$1"
  mkdir -p "$project"
  printf '%s\n' 'lint:' '	@true' 'test:' '	@false' > "$project/Makefile"
  printf '%s\n' '# Fixture' > "$project/README.md"
  printf '%s\n' 'console.log(1)' > "$project/app.js"
  git -C "$project" init -q -b main
  git -C "$project" -c user.name=t -c user.email=t@t add -A
  git -C "$project" -c user.name=t -c user.email=t@t commit -q -m base
  git -C "$project" switch -q -c work
  git -C "$project" branch -q --set-upstream-to=main
}

run_verify() {
  "$VERIFY" --project "$project" > "$TMP_ROOT/out" 2>&1
}

expect_docs_only() {
  run_verify || { cat "$TMP_ROOT/out"; fail "$1: docs-only verify failed"; }
  grep -q '^Scope: docs-only' "$TMP_ROOT/out" || fail "$1: scope was not docs-only"
  grep -q '^PASS: make:lint' "$TMP_ROOT/out" || fail "$1: lint did not run"
  if grep -q '==> make:test' "$TMP_ROOT/out"; then fail "$1: test ran on a docs-only change"; fi
}

expect_full() {
  if run_verify; then fail "$1: failing test target was skipped"; fi
  grep -q '^Scope: full' "$TMP_ROOT/out" || fail "$1: scope was not full"
  grep -q '^FAIL: make:test' "$TMP_ROOT/out" || fail "$1: test did not run"
}

new_project docs
printf '%s\n' 'More.' >> "$project/README.md"
mkdir -p "$project/docs"
printf '%s\n' '# New' > "$project/docs/new.md"
expect_docs_only "uncommitted and untracked docs"
grep -rqx 'SCOPE=docs-only' "$HARNESS_DB_ROOT" || fail "run record lacks SCOPE=docs-only"

new_project committed
printf '%s\n' 'More.' >> "$project/README.md"
git -C "$project" -c user.name=t -c user.email=t@t commit -q -am docs
expect_docs_only "committed docs"

new_project required
printf '%s\n' 'test' > "$project/.harness-required-checks"
git -C "$project" -c user.name=t -c user.email=t@t add -A
git -C "$project" -c user.name=t -c user.email=t@t commit -q -m required
git -C "$project" branch -q -f main HEAD
printf '%s\n' 'More.' >> "$project/README.md"
expect_docs_only "required test category"

new_project mixed
printf '%s\n' 'More.' >> "$project/README.md"
printf '%s\n' 'console.log(2)' >> "$project/app.js"
expect_full "docs plus code"

new_project rename
git -C "$project" mv app.js app.md
expect_full "code renamed to markdown"

new_project agents
printf '%s\n' '# Rules' > "$project/AGENTS.md"
expect_full "agent instruction file"

new_project nobase
git -C "$project" branch -q --unset-upstream
printf '%s\n' 'More.' >> "$project/README.md"
expect_full "no upstream base"

new_project clean
expect_full "no changes"

new_project forced
printf '%s\n' 'More.' >> "$project/README.md"
HARNESS_VERIFY_SCOPE=full
export HARNESS_VERIFY_SCOPE
expect_full "forced full scope"
unset HARNESS_VERIFY_SCOPE

new_project custom
printf '%s\n' 'docs/*' > "$project/.harness-docs-paths"
git -C "$project" -c user.name=t -c user.email=t@t add -A
git -C "$project" -c user.name=t -c user.email=t@t commit -q -m custom
git -C "$project" branch -q -f main HEAD
mkdir -p "$project/docs"
printf '%s\n' 'x' > "$project/docs/data.json"
expect_docs_only "project docs paths"
printf '%s\n' 'More.' >> "$project/README.md"
expect_full "markdown outside project docs paths"

printf '%s\n' 'PASS: docs-only verify scope'
