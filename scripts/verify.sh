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
  info "When every change since the upstream base is documentation (see"
  info ".harness-docs-paths), only format and lint run; HARNESS_VERIFY_SCOPE=full"
  info "forces every check."
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

# A registered target (scripts/harness-target.sh) owns its own database
# beneath the database root.
if [ -x "$SCRIPT_DIR/harness-target.sh" ]; then
  target_db=$(HARNESS_DB_ROOT=$HARNESS_DB_ROOT "$SCRIPT_DIR/harness-target.sh" db-root "$PROJECT_ROOT" 2>/dev/null || :)
  if [ -n "$target_db" ]; then
    HARNESS_DB_ROOT=$target_db
  fi
fi

if [ -z "$REQUIRED_CHECKS" ] && [ -f .harness-required-checks ]; then
  REQUIRED_CHECKS=$(sed 's/#.*//' .harness-required-checks | tr '\n' ' ')
  REQUIRED_CHECKS_SOURCE=".harness-required-checks"
fi
# scripts/init.sh records the categories it detected for a registered target in
# the target's own database, so a target needs no tracked file to be held to them.
if [ -z "$REQUIRED_CHECKS" ] && [ -f "$HARNESS_DB_ROOT/required-checks" ]; then
  REQUIRED_CHECKS=$(sed 's/#.*//' "$HARNESS_DB_ROOT/required-checks" | tr '\n' ' ')
  REQUIRED_CHECKS_SOURCE="$HARNESS_DB_ROOT/required-checks"
fi
REQUIRED_CHECKS=$(printf '%s' "$REQUIRED_CHECKS" | tr -s ' \t' '  ' | sed 's/^ //; s/ $//')

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
      format|lint|typecheck|test|build|allow-empty) ;;
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

# Docs-only scope: when every changed file is documentation, typecheck, test
# and build cannot be affected, so only format and lint run. Any doubt (no git,
# no base, empty change set, one non-doc path) falls back to the full run.
# Patterns come from .harness-docs-paths (one per line, `!` excludes) or the
# defaults below; HARNESS_VERIFY_SCOPE=full forces the full run.
SCOPE=full
SCOPE_REASON=""
DEFAULT_DOCS_PATHS='*.md
*.mdx
*.markdown
!AGENTS.md
!*/AGENTS.md
!CLAUDE.md
!*/CLAUDE.md
!SKILL.md
!*/SKILL.md'

docs_patterns() {
  if [ -f .harness-docs-paths ]; then
    sed 's/#.*//' .harness-docs-paths
  else
    printf '%s\n' "$DEFAULT_DOCS_PATHS"
  fi
}

is_doc_path() {
  path="$1"
  matched=1
  for pattern in $(docs_patterns); do
    case "$pattern" in
      !*)
        # shellcheck disable=SC2254
        case "$path" in ${pattern#!}) return 1 ;; esac
        ;;
      *)
        # shellcheck disable=SC2254
        case "$path" in $pattern) matched=0 ;; esac
        ;;
    esac
  done
  return "$matched"
}

changed_paths() {
  # Committed and uncommitted tracked changes since the base, with renames
  # split into old and new paths, plus untracked files.
  git diff --name-only --no-renames "$1" -- && git ls-files --others --exclude-standard
}

detect_scope() {
  # Patterns are matched with case, never expanded against the project's files.
  set -f
  detect_scope_paths
  set +f
}

detect_scope_paths() {
  SCOPE=full
  if [ "${HARNESS_VERIFY_SCOPE:-}" = "full" ]; then
    SCOPE_REASON="forced by HARNESS_VERIFY_SCOPE=full"
    return 0
  fi
  if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    SCOPE_REASON="not a git work tree"
    return 0
  fi
  base_ref=$(git rev-parse --verify -q '@{upstream}' 2>/dev/null || git rev-parse --verify -q refs/remotes/origin/HEAD 2>/dev/null || :)
  merge_base=""
  if [ -n "$base_ref" ]; then
    merge_base=$(git merge-base HEAD "$base_ref" 2>/dev/null || :)
  fi
  if [ -z "$merge_base" ]; then
    SCOPE_REASON="no upstream or origin/HEAD base"
    return 0
  fi
  if ! paths=$(changed_paths "$merge_base"); then
    SCOPE_REASON="could not list changed files"
    return 0
  fi
  if [ -z "$paths" ]; then
    SCOPE_REASON="no changed files since base"
    return 0
  fi
  count=0
  old_ifs=$IFS
  IFS='
'
  for path in $paths; do
    count=$((count + 1))
    if ! is_doc_path "$path"; then
      IFS=$old_ifs
      SCOPE_REASON="non-doc change: $path"
      return 0
    fi
  done
  IFS=$old_ifs
  SCOPE=docs-only
  SCOPE_REASON="$count changed file(s), all documentation"
}

skip_category_for_docs() {
  mark_skip "$1: docs-only change"
  if is_required "$1"; then
    info "REQUIRED: $1 not applicable to a docs-only change"
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

# Shell scripts that belong to this project, NUL-separated. Inside a git work tree the list
# comes from git (tracked plus untracked-but-not-ignored files), so ignored virtualenvs and
# nested repositories are skipped the same way git skips them. Outside git, fall back to a
# pruned find. Cached database and dependency directories are excluded in both modes.
list_shell_files() {
  # shellcheck disable=SC2016  # the quoted script below runs under the inner sh
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git ls-files -z --cached --others --exclude-standard -- '*.sh' 'scripts/harness' 2>/dev/null
  else
    find . \
      \( -path './.git' -o -path './.harness-db' -o -path './.venv' -o -path './vendor' -o -path './node_modules' -o -path './target' \) -prune \
      -o -type f \( -name '*.sh' -o -path './scripts/harness' \) -print0
  fi | xargs -0 sh -c '
    for f; do
      case "/$f" in
        */.harness-db/*|*/.venv/*|*/vendor/*|*/node_modules/*|*/target/*|*/.git/*) continue ;;
      esac
      [ -f "$f" ] && printf "%s\0" "$f"
    done
  ' _
}

has_shell_files() {
  [ -n "$(list_shell_files | tr '\0' x | head -c 1)" ]
}

shellcheck_shell_files() {
  list_shell_files | xargs -0 sh -c '[ "$#" -eq 0 ] || exec shellcheck "$@"' _
}

syntax_check_shell_files() {
  list_shell_files | xargs -0 sh -c '[ "$#" -eq 0 ] || exec sh -n "$@"' _
}

# Run this harness's own regression tests when verifying the harness itself.
run_harness_tests() {
  test_config=$(mktemp -d "${TMPDIR:-/tmp}/harness-test-config.XXXXXX") || return 1
  status=0
  for test_file in tests/*.sh; do
    info "--> $test_file"
    # Nested harness runs inside tests must not emit real Jev checkpoints, and
    # they model a human at a terminal, not the agent that may be running verify.
    if ! HARNESS_CONFIG_HOME="$test_config" HARNESS_JEV_CHECKPOINTS=0 HARNESS_JEV_DELEGATION=off HARNESS_SESSION_ID='' CODEX_THREAD_ID='' CLAUDE_SESSION_ID='' CLAUDE_CODE_SESSION_ID='' \
      CLAUDECODE='' CODEX_SANDBOX='' sh "$test_file"; then
      status=1
    fi
  done
  rm -rf "$test_config"
  return "$status"
}

# Shadow Jev checkpoint around verification: predicts the result before the
# checks run and labels that prediction from the real exit code afterwards.
# Enabled only by HARNESS_JEV_CHECKPOINTS=1; it never changes the exit code.
jev_checkpoint() {
  [ "${HARNESS_JEV_CHECKPOINTS:-0}" = "1" ] || return 0
  has_cmd python3 || return 0
  [ -f "$SCRIPT_DIR/phase_checkpoint.py" ] || return 0
  python3 "$SCRIPT_DIR/phase_checkpoint.py" "$@" --project "$PROJECT_ROOT" --db-root "$HARNESS_DB_ROOT" 2>/dev/null | while IFS= read -r jev_line; do
    info "Jev: $jev_line"
  done
  return 0
}

# Write a KEY=VALUE run record that `scripts/harness build done` requires.
write_run_record() {
  exit_code="$1"
  records_dir=$(sh "$SCRIPT_DIR/run-paths.sh" records "$HARNESS_DB_ROOT")
  if ! mkdir -p "$records_dir" 2>/dev/null; then
    info "WARN: could not create $records_dir; no run record written."
    return 0
  fi
  git_head=$(git -C "$PROJECT_ROOT" rev-parse HEAD 2>/dev/null || printf 'unknown')
  git_dirty=$(git -C "$PROJECT_ROOT" status --porcelain 2>/dev/null | grep -c . || true)
  tree_hash=$(sh "$SCRIPT_DIR/tree-hash.sh" "$PROJECT_ROOT")
  record="$records_dir/verify.state"
  current_file=$(sh "$SCRIPT_DIR/run-paths.sh" current "$HARNESS_DB_ROOT")
  run_id=$(head -n 1 "$current_file" 2>/dev/null || :)
  {
    printf 'RECORD_KIND=verify\n'
    printf 'RUN_ID=%s\n' "$run_id"
    printf 'RECORD_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'RECORD_EPOCH=%s\n' "$(date +%s)"
    printf 'PROJECT_ROOT=%s\n' "$PROJECT_ROOT"
    printf 'GIT_HEAD=%s\n' "$git_head"
    printf 'GIT_DIRTY_FILES=%s\n' "$git_dirty"
    printf 'TREE_HASH=%s\n' "$tree_hash"
    printf 'RAN=%s\n' "$ran"
    printf 'SKIPPED=%s\n' "$skipped"
    printf 'FAILURES=%s\n' "$failures"
    printf 'SCOPE=%s\n' "$SCOPE"
    printf 'EXIT=%s\n' "$exit_code"
  } > "$record.tmp.$$"
  mv "$record.tmp.$$" "$record"
  info "Run record: $record"
  if has_cmd python3; then
    python3 "$SCRIPT_DIR/workflow_audit.py" check --kind verify --record "$record" >&2 || :
  fi
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
  # Only check-style targets run here. `make format`/`make fmt` and a plain
  # `format` script usually rewrite files, and verification must never do that.
  if run_make_or_skip format-check || run_make_or_skip fmt-check || run_make_or_skip check-format; then
    return
  fi
  if make_has_target format || make_has_target fmt; then
    mark_skip "Makefile has format/fmt but no format-check target; verification never runs a rewriting formatter"
  fi

  ran_any=0

  pm="$(detect_node_pm)"
  if [ -n "$pm" ] && json_has_script package.json "format:check"; then
    run_node_script "$pm" "format:check"
    ran_any=1
  elif [ -n "$pm" ] && json_has_script package.json "prettier:check"; then
    run_node_script "$pm" "prettier:check"
    ran_any=1
  elif [ -n "$pm" ] && json_has_script package.json format; then
    mark_skip "package.json has a format script but no format:check; verification never runs a rewriting formatter"
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
      run_check "bash:shellcheck" shellcheck_shell_files
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
    run_check "bash:syntax" syntax_check_shell_files
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

jev_checkpoint verify-start

detect_scope
info "Scope: $SCOPE ($SCOPE_REASON)"

run_category format verify_format
run_category lint verify_lint
if [ "$SCOPE" = "docs-only" ]; then
  skip_category_for_docs typecheck
  skip_category_for_docs test
  skip_category_for_docs build
else
  run_category typecheck verify_typecheck
  run_category test verify_test
  run_category build verify_build
fi

if [ "$ran" -eq 0 ]; then
  if is_required allow-empty; then
    info ""
    info "NOTE: no checks ran; accepted because 'allow-empty' is declared ($REQUIRED_CHECKS_SOURCE)."
  else
    failures=$((failures + 1))
    info ""
    info "FAIL: no checks ran, so there is no evidence the change works."
    info "Add a check (make test, a package.json script, ...) or declare 'allow-empty' in .harness-required-checks."
  fi
fi

info ""
info "Verification summary: ran=$ran skipped=$skipped failures=$failures"

if [ "$failures" -ne 0 ]; then
  write_run_record 1
  jev_checkpoint verify-result --exit 1
  info "Verification failed."
  exit 1
fi

write_run_record 0
jev_checkpoint verify-result --exit 0
info "Verification passed."
