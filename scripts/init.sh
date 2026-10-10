#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"
ASSUME_YES="${HARNESS_INIT_YES:-0}"
AUTO=0
PLAN_FILE=""
TARGET_DIR=""

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/init.sh [--project PATH] [--yes] [--auto]"
  info ""
  info "Registers PATH as a harness target and bootstraps its dependencies."
  info "Defaults to the current directory. Project-owned commands (make init,"
  info "npm install, composer install, ...) are previewed first and only run after"
  info "you confirm, or with --yes / HARNESS_INIT_YES=1."
  info "--auto is the non-interactive session-start form: it runs the bootstrap"
  info "only when the project's manifests or lockfiles changed since the last"
  info "successful run, in the background unless HARNESS_AUTO_INIT_SYNC=1."
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
    --yes|-y)
      ASSUME_YES=1
      shift
      ;;
    --auto)
      AUTO=1
      ASSUME_YES=1
      shift
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

# Automatic mode initialises the whole project a session starts in: the git
# worktree or checkout root when PATH is inside one, otherwise PATH itself.
if [ "$AUTO" = "1" ] && [ -x "$SCRIPT_DIR/harness-target.sh" ]; then
  PROJECT_ROOT=$("$SCRIPT_DIR/harness-target.sh" root "$PROJECT_ROOT" 2>/dev/null || printf '%s' "$PROJECT_ROOT")
fi
cd "$PROJECT_ROOT" || exit 2

# Every initialisation registers the project as a harness target in the
# machine-local database so the CLI, verify, review, and hooks use its own
# state. The home directory, /, and the harness root itself are not targets.
if [ -x "$SCRIPT_DIR/harness-target.sh" ]; then
  TARGET_DIR=$("$SCRIPT_DIR/harness-target.sh" register "$PROJECT_ROOT" 2>/dev/null || :)
  # A refusal (the harness root, home, /) prints a reason, not a directory.
  [ -d "$TARGET_DIR" ] || TARGET_DIR=""
fi

PLAN_FILE=$(mktemp "${TMPDIR:-/tmp}/harness-init-plan.XXXXXX")
trap 'rm -f "$PLAN_FILE"' EXIT HUP INT TERM

# run_if_available DESC CMD...  Queues a project-owned command for the preview.
# Nothing runs until the plan is confirmed.
run_if_available() {
  desc="$1"
  shift
  printf '%s\t%s\n' "$desc" "$*" >> "$PLAN_FILE"
}

print_plan() {
  info ""
  info "Project-owned commands that would run in $PROJECT_ROOT:"
  while IFS="$(printf '\t')" read -r desc cmd; do
    info "  $cmd    # $desc"
  done < "$PLAN_FILE"
  info ""
  info "These come from the project's own files and run with your permissions."
}

confirm_plan() {
  if [ "$ASSUME_YES" = "1" ]; then
    return 0
  fi
  if [ ! -t 0 ]; then
    info "REFUSED: no terminal to confirm on. Re-run with --yes (or HARNESS_INIT_YES=1) to run them."
    exit 3
  fi
  printf 'Run them now? [y/N] '
  read -r answer
  case "$answer" in
    y|Y|yes|YES) ;;
    *)
      info "REFUSED: nothing was run."
      exit 3
      ;;
  esac
}

# run_plan  Executes the queued commands in order; returns the first failing exit code.
run_plan() {
  while IFS="$(printf '\t')" read -r desc cmd; do
    info "==> $desc"
    sh -c "$cmd" </dev/null || {
      _status=$?
      info "FAIL: $desc (exit $_status)"
      return "$_status"
    }
  done < "$PLAN_FILE"
  return 0
}

confirm_and_run_plan() {
  if [ ! -s "$PLAN_FILE" ]; then
    info "No project-owned setup commands detected; nothing to run."
    return 0
  fi
  print_plan
  confirm_plan
  run_plan || exit 1
}

# --- automatic mode ---------------------------------------------------------
# Used by scripts/hooks/auto-init.sh at every session start. The bootstrap
# state lives beside the target registration, never inside the project.

auto_say() {
  printf 'Harness auto-init: %s\n' "$*"
}

bootstrap_state_value() {
  sed -n "s/^$1=//p" "$BOOTSTRAP_STATE" 2>/dev/null | head -n 1
}

# write_bootstrap_state STATUS EXIT PID
write_bootstrap_state() {
  {
    printf 'STATUS=%s\n' "$1"
    printf 'FINGERPRINT=%s\n' "$FINGERPRINT"
    printf 'EXIT=%s\n' "$2"
    printf 'PID=%s\n' "$3"
    printf 'STARTED_AT=%s\n' "$STARTED_AT"
    printf 'UPDATED_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'LOG=%s\n' "$BOOTSTRAP_LOG"
    printf 'PROJECT_ROOT=%s\n' "$PROJECT_ROOT"
  } > "$BOOTSTRAP_STATE.tmp.$$"
  mv "$BOOTSTRAP_STATE.tmp.$$" "$BOOTSTRAP_STATE"
}

# run_bootstrap_worker  Runs the plan and records ok/failed; output goes to the caller's stdout.
run_bootstrap_worker() {
  STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  write_bootstrap_state running 0 "$$"
  info "AI development harness bootstrap (automatic)"
  info "Project root: $PROJECT_ROOT"
  print_plan
  _status=0
  run_plan || _status=$?
  if [ "$_status" -eq 0 ]; then
    write_bootstrap_state ok 0 ""
    info "Bootstrap completed."
    return 0
  fi
  write_bootstrap_state failed "$_status" ""
  info "Bootstrap failed (exit $_status)."
  return "$_status"
}

auto_mode() {
  if [ -z "$TARGET_DIR" ]; then
    # The harness root is already a harness root; it needs no registration
    # and no message on every session start.
    if [ "$PROJECT_ROOT" != "$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)" ]; then
      auto_say "$PROJECT_ROOT is not a registrable target (home directory or /); nothing to do."
    fi
    return 0
  fi
  BOOTSTRAP_STATE=$TARGET_DIR/bootstrap.state
  BOOTSTRAP_LOG=$TARGET_DIR/bootstrap.log
  FINGERPRINT=$("$SCRIPT_DIR/harness-target.sh" fingerprint "$PROJECT_ROOT" 2>/dev/null || printf 'unknown')
  STARTED_AT=$(bootstrap_state_value STARTED_AT)

  if [ "${HARNESS_AUTO_INIT_WORKER:-0}" = "1" ]; then
    run_bootstrap_worker
    return $?
  fi

  _previous_status=$(bootstrap_state_value STATUS)
  if [ "$(bootstrap_state_value FINGERPRINT)" = "$FINGERPRINT" ]; then
    case "$_previous_status" in
      ok)
        auto_say "target $PROJECT_ROOT registered ($TARGET_DIR); bootstrap current."
        return 0
        ;;
      running)
        _pid=$(bootstrap_state_value PID)
        if [ -n "$_pid" ] && kill -0 "$_pid" 2>/dev/null; then
          auto_say "target $PROJECT_ROOT registered; bootstrap still running (pid $_pid, log $BOOTSTRAP_LOG); wait for it before running project commands."
          return 0
        fi
        ;;
      failed)
        auto_say "target $PROJECT_ROOT registered; bootstrap failed earlier (exit $(bootstrap_state_value EXIT)); see $BOOTSTRAP_LOG or rerun scripts/init.sh --project $PROJECT_ROOT --yes."
        return 0
        ;;
    esac
  fi

  if [ ! -s "$PLAN_FILE" ]; then
    STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    write_bootstrap_state ok 0 ""
    auto_say "target $PROJECT_ROOT registered ($TARGET_DIR); no project-owned setup commands."
    return 0
  fi

  if [ "${HARNESS_AUTO_INIT_SYNC:-0}" = "1" ]; then
    _status=0
    run_bootstrap_worker > "$BOOTSTRAP_LOG" 2>&1 || _status=$?
    if [ "$_status" -eq 0 ]; then
      auto_say "target $PROJECT_ROOT registered ($TARGET_DIR); bootstrap completed (log $BOOTSTRAP_LOG)."
    else
      auto_say "target $PROJECT_ROOT registered; bootstrap FAILED (exit $_status); see $BOOTSTRAP_LOG or rerun scripts/init.sh --project $PROJECT_ROOT --yes."
    fi
    return 0
  fi

  STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  write_bootstrap_state running 0 ""
  HARNESS_AUTO_INIT_WORKER=1 nohup "$SCRIPT_DIR/init.sh" --project "$PROJECT_ROOT" --auto > "$BOOTSTRAP_LOG" 2>&1 < /dev/null &
  auto_say "target $PROJECT_ROOT registered ($TARGET_DIR); bootstrap started in background (pid $!, log $BOOTSTRAP_LOG); wait for it before running project commands."
  return 0
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

bootstrap_make() {
  if make_has_target init; then
    run_if_available "running make init" make init
  elif make_has_target setup; then
    run_if_available "running make setup" make setup
  elif [ -f Makefile ]; then
    info "Makefile found, but no init/setup target detected."
  fi
}

bootstrap_node() {
  pm="$(detect_node_pm)"
  if [ -n "$pm" ]; then
    case "$pm" in
      pnpm)
        run_if_available "installing JavaScript/TypeScript dependencies with pnpm" pnpm install
        ;;
      yarn)
        if [ -f yarn.lock ]; then
          run_if_available "installing JavaScript/TypeScript dependencies with yarn" yarn install --frozen-lockfile
        else
          run_if_available "installing JavaScript/TypeScript dependencies with yarn" yarn install
        fi
        ;;
      npm)
        if [ -f package-lock.json ]; then
          run_if_available "installing JavaScript/TypeScript dependencies with npm ci" npm ci
        else
          run_if_available "installing JavaScript/TypeScript dependencies with npm install" npm install
        fi
        ;;
    esac
  elif [ -f package.json ]; then
    info "package.json found, but npm/pnpm/yarn is unavailable."
  else
    info "No package.json found; skipping JavaScript/TypeScript dependency install."
  fi
}

bootstrap_php() {
  if [ -f composer.json ]; then
    if has_cmd composer; then
      if [ -f composer.lock ]; then
        run_if_available "installing PHP dependencies with composer install" composer install --no-interaction --prefer-dist
      else
        run_if_available "installing PHP dependencies with composer install" composer install --no-interaction
      fi
    else
      info "composer.json found, but composer is unavailable."
    fi
  else
    info "No composer.json found; skipping PHP dependency install."
  fi
}

bootstrap_go() {
  if [ -f go.mod ]; then
    if has_cmd go; then
      run_if_available "downloading Go modules" go mod download
    else
      info "go.mod found, but go is unavailable."
    fi
  else
    info "No go.mod found; skipping Go module download."
  fi
}

bootstrap_rust() {
  if [ -f Cargo.toml ]; then
    if has_cmd cargo; then
      if [ -f Cargo.lock ]; then
        run_if_available "fetching Rust dependencies with cargo" cargo fetch --locked
      else
        run_if_available "fetching Rust dependencies with cargo" cargo fetch
      fi
    else
      info "Cargo.toml found, but cargo is unavailable."
    fi
  else
    info "No Cargo.toml found; skipping Rust dependency fetch."
  fi
}

if [ "$AUTO" != "1" ]; then
  info "AI development harness bootstrap"
  info "Project root: $PROJECT_ROOT"
  if [ -n "$TARGET_DIR" ]; then
    info "Registered harness target: $TARGET_DIR"
  fi
  info ""
fi

missing=0
for tool in git sh; do
  if has_cmd "$tool"; then
    [ "$AUTO" = "1" ] || info "found: $tool"
  else
    info "missing required tool: $tool"
    missing=1
  fi
done

if [ "$missing" -ne 0 ]; then
  info ""
  info "Install missing required tools and rerun scripts/init.sh."
  exit 1
fi

bootstrap_make
bootstrap_node
bootstrap_php
bootstrap_go
bootstrap_rust

package_script() {
  [ -f "$1" ] && grep -q "\"$2\"[[:space:]]*:" "$1"
}

# record_required_checks  Holds a registered target to the check categories it
# has tooling for, so verification cannot pass with nothing checked. The record
# lives in the target's database, never in the project; a project file
# (.harness-required-checks) or an existing record wins.
record_required_checks() {
  [ -n "$TARGET_DIR" ] || return 0
  [ ! -f "$PROJECT_ROOT/.harness-required-checks" ] || return 0
  _record=$TARGET_DIR/db/required-checks
  [ ! -f "$_record" ] || return 0
  _categories=""
  if make_has_target test || package_script package.json test || package_script composer.json test \
    || [ -f go.mod ] || [ -f Cargo.toml ]; then
    _categories="test"
  fi
  if make_has_target lint || package_script package.json lint || package_script composer.json lint \
    || [ -f go.mod ] || [ -f Cargo.toml ]; then
    _categories="$_categories lint"
  fi
  [ -n "$_categories" ] || return 0
  mkdir -p "$TARGET_DIR/db"
  {
    printf '%s\n' "# Detected by scripts/init.sh; edit or delete to change. allow-empty accepts a run with no checks."
    for _category in $_categories; do
      printf '%s\n' "$_category"
    done
  } > "$_record"
  [ "$AUTO" = "1" ] || info "Required checks recorded for this target: $_categories ($_record)"
}

# write_project_map  A short map of the target, regenerated at every init: where
# things are, which checks exist, and which docs to open. It lives beside the
# target registration and is named in the session brief, so an agent reads one
# small file and then only what the task needs.
write_project_map() {
  [ -n "$TARGET_DIR" ] || return 0
  _map=$TARGET_DIR/map.md
  {
    printf '# Project map: %s\n\n' "$PROJECT_ROOT"
    printf '%s\n' "Generated by scripts/init.sh; read the files it names, not everything."
    printf '\n%s\n' "## Top level (files tracked under each)"
    if git -C "$PROJECT_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
      git -C "$PROJECT_ROOT" ls-files | awk -F/ '{ top = (NF > 1) ? $1 "/" : $1; count[top]++ } END { for (t in count) printf "- %s (%d)\n", t, count[t] }' | sort | head -n 40
    else
      for _entry in "$PROJECT_ROOT"/*; do
        [ -e "$_entry" ] || continue
        if [ -d "$_entry" ]; then printf -- '- %s/\n' "${_entry##*/}"; else printf -- '- %s\n' "${_entry##*/}"; fi
      done | head -n 40
    fi
    printf '\n%s\n' "## Checks scripts/verify.sh can run"
    for _target in format-check fmt-check lint typecheck type-check test build; do
      make_has_target "$_target" && printf -- '- make %s\n' "$_target"
    done
    for _script in format:check lint typecheck test build; do
      package_script package.json "$_script" && printf -- '- package.json script %s\n' "$_script"
    done
    [ -f go.mod ] && printf '%s\n' "- go vet / go test / go build"
    [ -f Cargo.toml ] && printf '%s\n' "- cargo clippy / cargo test / cargo build"
    [ -f "$TARGET_DIR/db/required-checks" ] && printf -- '- required: %s\n' "$(grep -v '^#' "$TARGET_DIR/db/required-checks" | tr '\n' ' ')"
    printf '\n%s\n' "## Docs to start from"
    for _doc in README.md AGENTS.md CLAUDE.md CONTRIBUTING.md docs/*.md; do
      [ -f "$_doc" ] && printf -- '- %s\n' "$_doc"
    done | head -n 20
  } > "$_map.tmp.$$" 2>/dev/null
  mv "$_map.tmp.$$" "$_map"
}

record_required_checks
write_project_map

if [ "$AUTO" = "1" ]; then
  auto_mode
  exit $?
fi

confirm_and_run_plan

info ""
info "Next steps:"
info "1. Read AGENTS.md, CLAUDE.md, docs/architecture.md, docs/conventions.md, and docs/setup.md."
info "2. Create a task from tasks/task-template.md."
info "3. Run scripts/verify.sh before declaring work complete."
