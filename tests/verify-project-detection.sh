#!/usr/bin/env sh
set -eu

# Node, PHP, Go, and Rust detection in scripts/verify.sh. Each fixture is
# verified with a sandboxed PATH: the few system utilities verify needs, plus
# recording stubs in place of the real toolchains. The stubs log every call,
# so the tests check exactly which commands each project type runs without any
# real toolchain. The one exception is the cases that read package.json through
# node: they run only when node is installed, and print a SKIP line otherwise,
# in which case only the grep fallback is covered.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
VERIFY="$HARNESS_ROOT/scripts/verify.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-detection.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# Keep fixture run records out of the real harness database.
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
# Each fixture declares its own requirements; a caller's override must not leak in.
unset HARNESS_REQUIRED_CHECKS
OUT="$TMP_ROOT/output"
STUB_CALLS="$TMP_ROOT/calls"
export STUB_CALLS
BASE_BIN="$TMP_ROOT/base-bin"
STUB="$TMP_ROOT/stub"
GOFMT_STUB="$TMP_ROOT/gofmt-stub"

fail() {
  printf 'FAIL: %s\n' "$1"
  if [ -f "$OUT" ]; then
    printf '%s\n' '--- verify output ---'
    cat "$OUT"
  fi
  if [ -f "$STUB_CALLS" ]; then
    printf '%s\n' '--- stub calls ---'
    cat "$STUB_CALLS"
  fi
  exit 1
}

# Utilities verify.sh itself needs. Anything else, make and node included, is
# absent unless a case adds it on purpose.
mkdir -p "$BASE_BIN"
for tool in sh find grep sed tr date mkdir mv cat head cut dirname; do
  real=$(command -v "$tool") || fail "host utility $tool is unavailable"
  ln -s "$real" "$BASE_BIN/$tool"
done

# A toolchain stub: logs "name args" and exits 1 when the call matches the
# STUB_FAIL glob.
cat > "$STUB" <<'EOF'
#!/bin/sh
call="${0##*/} $*"
printf '%s\n' "$call" >> "$STUB_CALLS"
case "$call" in
  ${STUB_FAIL:-__no_failure__}) exit 1 ;;
esac
exit 0
EOF
# gofmt reports unformatted files on stdout rather than through its exit code.
cat > "$GOFMT_STUB" <<'EOF'
#!/bin/sh
printf '%s\n' "gofmt $*" >> "$STUB_CALLS"
for file in "$@"; do
  case "$file" in
    *unformatted.go) printf '%s\n' "$file" ;;
  esac
done
exit 0
EOF
chmod +x "$STUB" "$GOFMT_STUB"

# stub_bin NAME TOOL...: a directory holding stubs for TOOL...; prints its path.
stub_bin() {
  dir="$TMP_ROOT/bin-$1"
  shift
  mkdir -p "$dir"
  for tool in "$@"; do
    case "$tool" in
      gofmt) ln -s "$GOFMT_STUB" "$dir/gofmt" ;;
      *) ln -s "$STUB" "$dir/$tool" ;;
    esac
  done
  printf '%s\n' "$dir"
}

# run_verify BIN PROJECT [FAIL_GLOB]: verify PROJECT with BIN's stubs and the
# base utilities as the whole PATH; sets rc to verify's exit code.
run_verify() {
  : > "$STUB_CALLS"
  rc=0
  PATH="$1:$BASE_BIN" STUB_FAIL="${3:-}" "$VERIFY" --project "$2" > "$OUT" 2>&1 || rc=$?
}

# fixture NAME: a fresh empty project directory; prints its path.
fixture() {
  dir="$TMP_ROOT/project-$1"
  mkdir -p "$dir"
  printf '%s\n' "$dir"
}

expect_rc() {
  [ "$rc" -eq "$1" ] || fail "$2: verify exited $rc, expected $1"
}

expect_line() {
  grep -q -x -F "$1" "$OUT" || fail "$2: missing output line: $1"
}

# expect_calls CONTEXT CALL...: the stubs saw exactly CALL..., in this order.
expect_calls() {
  context="$1"
  shift
  expected=$(printf '%s\n' "$@")
  actual=$(cat "$STUB_CALLS")
  [ "$actual" = "$expected" ] || fail "$context: unexpected toolchain calls"
}

node_bin=$(stub_bin node npm pnpm yarn)
php_bin=$(stub_bin php composer)
go_bin=$(stub_bin go go gofmt)
rust_bin=$(stub_bin rust cargo)
all_bin=$(stub_bin all npm pnpm yarn composer go gofmt cargo)
none_bin=$(stub_bin none)

# --- No manifest: every category is an explicit skip and no toolchain runs.
empty=$(fixture empty)
run_verify "$all_bin" "$empty"
expect_rc 0 'empty project'
expect_calls 'empty project'
for line in 'SKIP: no formatter/check target detected' 'SKIP: no lint target detected' \
  'SKIP: no typecheck/static-analysis target detected' 'SKIP: no test target detected' \
  'SKIP: no build target detected'; do
  expect_line "$line" 'empty project'
done

# --- Node: package-lock.json selects npm, and each category runs its script.
node=$(fixture node)
printf '%s\n' '{"name":"fixture","private":true,"scripts":{"format:check":"x","lint":"x","typecheck":"x","test":"x","build":"x"}}' > "$node/package.json"
printf '%s\n' '{}' > "$node/package-lock.json"
node_calls() {
  expect_calls "$1" 'npm run format:check' 'npm run lint' 'npm run typecheck' 'npm test' 'npm run build'
  for check in format:check lint typecheck test build; do
    expect_line "PASS: node:$check" "$1"
  done
}
run_verify "$node_bin" "$node"
expect_rc 0 'node project'
node_calls 'node project'

# With node available, verify reads the scripts through node instead of grep,
# and still runs only the scripts that exist.
if real_node=$(command -v node); then
  with_node=$(stub_bin with-node npm pnpm yarn)
  ln -s "$real_node" "$with_node/node"
  run_verify "$with_node" "$node"
  expect_rc 0 'node project read by node'
  node_calls 'node project read by node'
  node_test_only=$(fixture node-test-only)
  printf '%s\n' '{"name":"fixture","private":true,"scripts":{"test":"x"}}' > "$node_test_only/package.json"
  : > "$node_test_only/package-lock.json"
  run_verify "$with_node" "$node_test_only"
  expect_rc 0 'node test-only project read by node'
  expect_calls 'node test-only project read by node' 'npm test'
else
  printf '%s\n' 'SKIP: node is unavailable; package.json read through node not tested (grep fallback only)'
fi

# A failing script fails verification; the later categories still run.
run_verify "$node_bin" "$node" 'npm test'
expect_rc 1 'failing npm test'
expect_line 'FAIL: node:test (exit 1)' 'failing npm test'
expect_line 'PASS: node:build' 'failing npm test'
expect_line 'Verification summary: ran=5 skipped=0 failures=1' 'failing npm test'

# The lockfile picks the package manager; without one, pnpm, yarn, npm in turn.
for case in pnpm-lock.yaml:pnpm yarn.lock:yarn package-lock.json:npm none:pnpm; do
  lockfile=${case%%:*}
  pm=${case#*:}
  project=$(fixture "node-$lockfile")
  printf '%s\n' '{"name":"fixture","private":true,"scripts":{"test":"x"}}' > "$project/package.json"
  [ "$lockfile" = none ] || : > "$project/$lockfile"
  run_verify "$node_bin" "$project"
  expect_rc 0 "node lockfile $lockfile"
  expect_calls "node lockfile $lockfile" "$pm test"
done

# A rewriting format script is never run.
node_format=$(fixture node-format)
printf '%s\n' '{"name":"fixture","private":true,"scripts":{"format":"x"}}' > "$node_format/package.json"
run_verify "$node_bin" "$node_format"
expect_rc 0 'node rewriting format script'
expect_calls 'node rewriting format script'
expect_line 'SKIP: package.json has a format script but no format:check; verification never runs a rewriting formatter' 'node rewriting format script'

# No package manager: nothing runs, and a required test category fails.
printf '%s\n' test > "$node/.harness-required-checks"
run_verify "$none_bin" "$node"
expect_rc 1 'node without a package manager'
expect_calls 'node without a package manager'
expect_line "FAIL: required category 'test' ran no checks" 'node without a package manager'
rm "$node/.harness-required-checks"

# --- PHP: composer scripts for lint, static analysis, test, and build.
php=$(fixture php)
printf '%s\n' '{"name":"fixture/php","scripts":{"lint":"x","analyse":"x","test":"x","build":"x"}}' > "$php/composer.json"
run_verify "$php_bin" "$php"
expect_rc 0 'php project'
expect_calls 'php project' 'composer run-script lint' 'composer run-script analyse' 'composer run-script test' 'composer run-script build'
for check in lint analyse test build; do
  expect_line "PASS: php:$check" 'php project'
done
expect_line 'SKIP: no formatter/check target detected' 'php project'

run_verify "$php_bin" "$php" 'composer run-script analyse'
expect_rc 1 'failing composer analyse'
expect_line 'FAIL: php:analyse (exit 1)' 'failing composer analyse'
expect_line 'PASS: php:test' 'failing composer analyse'

# Static analysis runs one script: phpstan before psalm.
php_phpstan=$(fixture php-phpstan)
printf '%s\n' '{"name":"fixture/php","scripts":{"psalm":"x","phpstan":"x"}}' > "$php_phpstan/composer.json"
run_verify "$php_bin" "$php_phpstan"
expect_rc 0 'php phpstan and psalm'
expect_calls 'php phpstan and psalm' 'composer run-script phpstan'

printf '%s\n' test > "$php/.harness-required-checks"
run_verify "$none_bin" "$php"
expect_rc 1 'php without composer'
expect_calls 'php without composer'
for purpose in lint 'static analysis' tests build; do
  expect_line "SKIP: composer.json found, but composer is unavailable for $purpose" 'php without composer'
done
expect_line "FAIL: required category 'test' ran no checks" 'php without composer'
rm "$php/.harness-required-checks"

# --- Go: gofmt outside vendor/, vet, test compile, test, and build.
go=$(fixture go)
mkdir -p "$go/vendor/dep"
printf '%s\n' 'module example.com/fixture' > "$go/go.mod"
printf '%s\n' 'package main' > "$go/main.go"
printf '%s\n' 'package dep' > "$go/vendor/dep/dep.go"
run_verify "$go_bin" "$go"
expect_rc 0 'go project'
expect_calls 'go project' 'gofmt -l ./main.go' 'go vet ./...' 'go test ./... -run ^$' 'go test ./...' 'go build ./...'
for check in fmt vet test-compile test build; do
  expect_line "PASS: go:$check" 'go project'
done

# gofmt listing a file fails the format check.
printf '%s\n' 'package main' > "$go/unformatted.go"
run_verify "$go_bin" "$go"
expect_rc 1 'unformatted go file'
expect_line 'FAIL: go:fmt (exit 1)' 'unformatted go file'
rm "$go/unformatted.go"

# A failing test run is reported apart from the compile-only pass.
run_verify "$go_bin" "$go" 'go test ./...'
expect_rc 1 'failing go test'
expect_line 'PASS: go:test-compile' 'failing go test'
expect_line 'FAIL: go:test (exit 1)' 'failing go test'

printf '%s\n' build > "$go/.harness-required-checks"
run_verify "$none_bin" "$go"
expect_rc 1 'go without go'
expect_calls 'go without go'
for purpose in 'format check' lint tests build; do
  expect_line "SKIP: go.mod found, but go is unavailable for $purpose" 'go without go'
done
expect_line "FAIL: required category 'build' ran no checks" 'go without go'
rm "$go/.harness-required-checks"

# --- Rust: cargo fmt check, clippy, check, test, and build.
rust=$(fixture rust)
printf '%s\n' '[package]' 'name = "fixture"' 'version = "0.1.0"' > "$rust/Cargo.toml"
run_verify "$rust_bin" "$rust"
expect_rc 0 'rust project'
expect_calls 'rust project' 'cargo fmt --check' 'cargo clippy --all-targets --all-features -- -D warnings' \
  'cargo check --all-targets --all-features' 'cargo test --all-targets --all-features' 'cargo build --all-targets --all-features'
for check in fmt clippy check test build; do
  expect_line "PASS: rust:$check" 'rust project'
done

run_verify "$rust_bin" "$rust" 'cargo clippy *'
expect_rc 1 'failing cargo clippy'
expect_line 'FAIL: rust:clippy (exit 1)' 'failing cargo clippy'
expect_line 'PASS: rust:check' 'failing cargo clippy'

printf '%s\n' lint > "$rust/.harness-required-checks"
run_verify "$none_bin" "$rust"
expect_rc 1 'rust without cargo'
expect_calls 'rust without cargo'
for purpose in 'format check' lint tests build; do
  expect_line "SKIP: Cargo.toml found, but cargo is unavailable for $purpose" 'rust without cargo'
done
expect_line "FAIL: required category 'lint' ran no checks" 'rust without cargo'
rm "$rust/.harness-required-checks"

printf '%s\n' 'PASS: Node, PHP, Go, and Rust detection'
