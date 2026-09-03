#!/usr/bin/env sh
set -u

failures=0
ran=0
skipped=0
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HARNESS_DB_ROOT="${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}"
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"
REQUIRED_CHECKS="${HARNESS_REQUIRED_CHECKS:-}"
REQUIRED_CHECKS_SOURCE="environment"

usage() {
  info "Usage: scripts/verify.sh [--project PATH]"
  info ""
  info "Runs verification sensors in PATH. Defaults to the current directory."
}

info() {
  printf '%s\n' "$*"
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --project)
      if [ "$#" -lt 2 ]; then
        info "FAIL: --project requires a path."
        exit 2
      fi
      PROJECT_ROOT="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      info "FAIL: unknown argument: $1"
      usage
      exit 2
      ;;
  esac
done

if [ ! -d "$PROJECT_ROOT" ]; then
  info "FAIL: project root does not exist or is not a directory: $PROJECT_ROOT"
  exit 2
fi

PROJECT_ROOT=$(cd "$PROJECT_ROOT" && pwd -P)
cd "$PROJECT_ROOT" || exit 2

if [ -z "$REQUIRED_CHECKS" ] && [ -f .harness-required-checks ]; then
  REQUIRED_CHECKS=$(sed 's/#.*//' .harness-required-checks | tr '\n' ' ')
  REQUIRED_CHECKS_SOURCE=".harness-required-checks"
fi

is_required() {
  wanted="$1"
  for required in $REQUIRED_CHECKS; do
    if [ "$required" = "$wanted" ]; then
      return 0
    fi
  done
  return 1
}

validate_required_checks() {
  for required in $REQUIRED_CHECKS; do
    case "$required" in
      format|lint|typecheck|test|build) ;;
      *)
        info "FAIL: unknown required check category: $required"
        exit 2
        ;;
    esac
  done
}

mark_skip() {
  skipped=$((skipped + 1))
  info "SKIP: $*"
}

run_check() {
  name="$1"
  shift
  ran=$((ran + 1))
  info ""
  info "==> $name"
  if "$@"; then
    info "PASS: $name"
  else
    code=$?
    failures=$((failures + 1))
    info "FAIL: $name (exit $code)"
  fi
}

run_category() {
  category="$1"
  shift
  ran_before=$ran
  failures_before=$failures

  "$@"

  if is_required "$category" && [ "$ran" -eq "$ran_before" ]; then
    failures=$((failures + 1))
    info "FAIL: required category '$category' ran no checks"
  elif is_required "$category" && [ "$failures" -eq "$failures_before" ]; then
    info "REQUIRED: $category satisfied"
  fi
}

make_has_target() {
  [ -f Makefile ] && has_cmd make && make -qp 2>/dev/null | grep -q "^$1:"
}

detect_node_pm() {
  if [ -f pnpm-lock.yaml ] && has_cmd pnpm; then
    printf '%s\n' pnpm
  elif [ -f yarn.lock ] && has_cmd yarn; then
    printf '%s\n' yarn
  elif [ -f package-lock.json ] && has_cmd npm; then
    printf '%s\n' npm
  elif [ -f package.json ]; then
    if has_cmd pnpm; then
      printf '%s\n' pnpm
    elif has_cmd yarn; then
      printf '%s\n' yarn
    elif has_cmd npm; then
      printf '%s\n' npm
    else
      printf '%s\n' ""
    fi
  else
    printf '%s\n' ""
  fi
}

json_has_script() {
  file="$1"
  script="$2"
  [ -f "$file" ] || return 1
  if has_cmd node; then
    node -e "const p=require('./$file'); process.exit(p.scripts && p.scripts['$script'] ? 0 : 1)"
  else
    grep -q "\"$script\"[[:space:]]*:" "$file"
  fi
}

run_node_script() {
  pm="$1"
  script="$2"
  case "$pm" in
    pnpm) run_check "node:$script" pnpm run "$script" ;;
    yarn) run_check "node:$script" yarn run "$script" ;;
    npm) run_check "node:$script" npm run "$script" ;;
  esac
}

run_composer_script() {
  script="$1"
  run_check "php:$script" composer run-script "$script"
}

check_go_format() {
  out=$(
    find . \
      -path ./.git -prune \
      -o -path ./vendor -prune \
      -o -name "*.go" -exec gofmt -l {} +
  )
  test -z "$out"
}

has_shell_files() {
  find . \
    \( -path './.git' -o -path './.venv' -o -path './vendor' -o -path './node_modules' -o -path './target' \) -prune \
    -o -type f \( -name '*.sh' -o -path './scripts/*' \) -print -quit | grep -q .
}

# Run this harness's own regression tests when verifying the harness itself.
run_harness_tests() {
  status=0
  for test_file in tests/*.sh; do
    info "--> $test_file"
    if ! sh "$test_file"; then
      status=1
    fi
  done
  return "$status"
}

# Write a KEY=VALUE run record that `scripts/harness build done` requires.
write_run_record() {
  exit_code="$1"
  records_dir="$HARNESS_DB_ROOT/records"
  if ! mkdir -p "$records_dir" 2>/dev/null; then
    info "WARN: could not create $records_dir; no run record written."
    return 0
  fi
  git_head=$(git -C "$PROJECT_ROOT" rev-parse HEAD 2>/dev/null || printf 'unknown')
  git_dirty=$(git -C "$PROJECT_ROOT" status --porcelain 2>/dev/null | grep -c . || true)
  record="$records_dir/verify.state"
  {
    printf 'RECORD_KIND=verify\n'
    printf 'RECORD_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'RECORD_EPOCH=%s\n' "$(date +%s)"
    printf 'PROJECT_ROOT=%s\n' "$PROJECT_ROOT"
    printf 'GIT_HEAD=%s\n' "$git_head"
    printf 'GIT_DIRTY_FILES=%s\n' "$git_dirty"
    printf 'RAN=%s\n' "$ran"
    printf 'SKIPPED=%s\n' "$skipped"
    printf 'FAILURES=%s\n' "$failures"
    printf 'EXIT=%s\n' "$exit_code"
  } > "$record.tmp.$$"
  mv "$record.tmp.$$" "$record"
  info "Run record: $record"
}

run_make_or_skip() {
  target="$1"
  if make_has_target "$target"; then
    run_check "make:$target" make "$target"
    return 0
  fi
  return 1
}

verify_format() {
  if run_make_or_skip format || run_make_or_skip fmt; then
    return
  fi

  ran_any=0

  pm="$(detect_node_pm)"
  if [ -n "$pm" ] && json_has_script package.json format; then
    run_node_script "$pm" format
    ran_any=1
  elif [ -n "$pm" ] && json_has_script package.json "format:check"; then
    run_node_script "$pm" "format:check"
    ran_any=1
  fi

  if [ -f go.mod ] && has_cmd go; then
    run_check "go:fmt" check_go_format
    ran_any=1
  elif [ -f go.mod ]; then
    mark_skip "go.mod found, but go is unavailable for format check"
    ran_any=1
  fi

  if [ -f Cargo.toml ] && has_cmd cargo; then
    run_check "rust:fmt" cargo fmt --check
    ran_any=1
  elif [ -f Cargo.toml ]; then
    mark_skip "Cargo.toml found, but cargo is unavailable for format check"
    ran_any=1
  fi

  if [ "$ran_any" -eq 0 ]; then
    mark_skip "no formatter/check target detected"
  fi
}

verify_lint() {
  if run_make_or_skip lint; then
    return
  fi

  ran_any=0

  pm="$(detect_node_pm)"
  if [ -n "$pm" ] && json_has_script package.json lint; then
    run_node_script "$pm" lint
    ran_any=1
  fi

  if [ -f composer.json ] && has_cmd composer && json_has_script composer.json lint; then
    run_composer_script lint
    ran_any=1
  elif [ -f composer.json ] && ! has_cmd composer; then
    mark_skip "composer.json found, but composer is unavailable for lint"
    ran_any=1
  fi

  if [ -f go.mod ] && has_cmd go; then
    run_check "go:vet" go vet ./...
    ran_any=1
  elif [ -f go.mod ]; then
    mark_skip "go.mod found, but go is unavailable for lint"
    ran_any=1
  fi

  if [ -f Cargo.toml ] && has_cmd cargo; then
    run_check "rust:clippy" cargo clippy --all-targets --all-features -- -D warnings
    ran_any=1
  elif [ -f Cargo.toml ]; then
    mark_skip "Cargo.toml found, but cargo is unavailable for lint"
    ran_any=1
  fi

  if has_shell_files; then
    if has_cmd shellcheck; then
      run_check "bash:shellcheck" find . \( -path './.git' -o -path './.venv' -o -path './vendor' -o -path './node_modules' -o -path './target' \) -prune -o -type f \( -name '*.sh' -o -path './scripts/*' \) -exec shellcheck {} +
    else
      mark_skip "shell scripts found, but shellcheck is unavailable"
    fi
    ran_any=1
  fi

  if [ "$ran_any" -eq 0 ]; then
    mark_skip "no lint target detected"
  fi
}

verify_typecheck() {
  if run_make_or_skip typecheck || run_make_or_skip type-check; then
    return
  fi

  ran_any=0

  pm="$(detect_node_pm)"
  if [ -n "$pm" ] && json_has_script package.json typecheck; then
    run_node_script "$pm" typecheck
    ran_any=1
  elif [ -n "$pm" ] && json_has_script package.json "type-check"; then
    run_node_script "$pm" "type-check"
    ran_any=1
  fi

  if [ -f composer.json ] && has_cmd composer && json_has_script composer.json analyse; then
    run_composer_script analyse
    ran_any=1
  elif [ -f composer.json ] && has_cmd composer && json_has_script composer.json analyze; then
    run_composer_script analyze
    ran_any=1
  elif [ -f composer.json ] && has_cmd composer && json_has_script composer.json phpstan; then
    run_composer_script phpstan
    ran_any=1
  elif [ -f composer.json ] && has_cmd composer && json_has_script composer.json psalm; then
    run_composer_script psalm
    ran_any=1
  elif [ -f composer.json ] && ! has_cmd composer; then
    mark_skip "composer.json found, but composer is unavailable for static analysis"
    ran_any=1
  fi

  if [ -f go.mod ] && has_cmd go; then
    run_check "go:test-compile" go test ./... -run '^$'
    ran_any=1
  fi

  if [ -f Cargo.toml ] && has_cmd cargo; then
    run_check "rust:check" cargo check --all-targets --all-features
    ran_any=1
  fi

  if [ "$ran_any" -eq 0 ]; then
    mark_skip "no typecheck/static-analysis target detected"
  fi
}

verify_test() {
  if run_make_or_skip test; then
    return
  fi

  ran_any=0

  pm="$(detect_node_pm)"
  if [ -n "$pm" ] && json_has_script package.json test; then
    case "$pm" in
      pnpm) run_check "node:test" pnpm test ;;
      yarn) run_check "node:test" yarn test ;;
      npm) run_check "node:test" npm test ;;
    esac
    ran_any=1
  fi

  if [ -f composer.json ] && has_cmd composer && json_has_script composer.json test; then
    run_composer_script test
    ran_any=1
  elif [ -f composer.json ] && ! has_cmd composer; then
    mark_skip "composer.json found, but composer is unavailable for tests"
    ran_any=1
  fi

  if [ -f go.mod ] && has_cmd go; then
    run_check "go:test" go test ./...
    ran_any=1
  elif [ -f go.mod ]; then
    mark_skip "go.mod found, but go is unavailable for tests"
    ran_any=1
  fi

  if [ -f Cargo.toml ] && has_cmd cargo; then
    run_check "rust:test" cargo test --all-targets --all-features
    ran_any=1
  elif [ -f Cargo.toml ]; then
    mark_skip "Cargo.toml found, but cargo is unavailable for tests"
    ran_any=1
  fi

  if [ -x scripts/harness ] && ls tests/*.sh >/dev/null 2>&1; then
    run_check "harness:tests" run_harness_tests
    ran_any=1
  fi

  if has_shell_files; then
    run_check "bash:syntax" find . \( -path './.git' -o -path './.venv' -o -path './vendor' -o -path './node_modules' -o -path './target' \) -prune -o -type f \( -name '*.sh' -o -path './scripts/*' \) -exec sh -n {} +
    ran_any=1
  fi

  if [ "$ran_any" -eq 0 ]; then
    mark_skip "no test target detected"
  fi
}

verify_build() {
  if run_make_or_skip build; then
    return
  fi

  ran_any=0

  pm="$(detect_node_pm)"
  if [ -n "$pm" ] && json_has_script package.json build; then
    run_node_script "$pm" build
    ran_any=1
  fi

  if [ -f composer.json ] && has_cmd composer && json_has_script composer.json build; then
    run_composer_script build
    ran_any=1
  elif [ -f composer.json ] && ! has_cmd composer; then
    mark_skip "composer.json found, but composer is unavailable for build"
    ran_any=1
  fi

  if [ -f go.mod ] && has_cmd go; then
    run_check "go:build" go build ./...
    ran_any=1
  elif [ -f go.mod ]; then
    mark_skip "go.mod found, but go is unavailable for build"
    ran_any=1
  fi

  if [ -f Cargo.toml ] && has_cmd cargo; then
    run_check "rust:build" cargo build --all-targets --all-features
    ran_any=1
  elif [ -f Cargo.toml ]; then
    mark_skip "Cargo.toml found, but cargo is unavailable for build"
    ran_any=1
  fi

  if [ "$ran_any" -eq 0 ]; then
    mark_skip "no build target detected"
  fi
}

info "Verification started"
info "Project root: $PROJECT_ROOT"

validate_required_checks
if [ -n "$REQUIRED_CHECKS" ]; then
  info "Required checks ($REQUIRED_CHECKS_SOURCE): $REQUIRED_CHECKS"
else
  info "Required checks: none declared"
fi

run_category format verify_format
run_category lint verify_lint
run_category typecheck verify_typecheck
run_category test verify_test
run_category build verify_build

info ""
info "Verification summary: ran=$ran skipped=$skipped failures=$failures"

if [ "$failures" -ne 0 ]; then
  write_run_record 1
  info "Verification failed."
  exit 1
fi

write_run_record 0
info "Verification passed."
