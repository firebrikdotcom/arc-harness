#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"
ASSUME_YES="${HARNESS_INIT_YES:-0}"
AUTO=0
PLAN_FILE=""
TARGET_DIR=""
# Set when detection finds a setup the bootstrap must not guess at; nothing runs.
BOOTSTRAP_REFUSAL=""

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

is_positive_int() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$1" -gt 0 ] 2>/dev/null
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

# Each project-owned command gets this many seconds before it is stopped.
BOOTSTRAP_TIMEOUT="${HARNESS_BOOTSTRAP_TIMEOUT:-1800}"
if ! is_positive_int "$BOOTSTRAP_TIMEOUT"; then
  info "FAIL: HARNESS_BOOTSTRAP_TIMEOUT must be a whole number of seconds greater than 0, got '$BOOTSTRAP_TIMEOUT'."
  exit 2
fi

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
fi

PLAN_FILE=$(mktemp "${TMPDIR:-/tmp}/harness-init-plan.XXXXXX")
MAKE_DB="$PLAN_FILE.make"
trap 'rm -f "$PLAN_FILE" "$MAKE_DB"' EXIT HUP INT TERM

# Project commands and project-controlled probes run only under a timeout
# supervisor; without one the bootstrap refuses instead of running unbounded.
SUPERVISED=1
"$SCRIPT_DIR/with-timeout.sh" --check || SUPERVISED=0
NO_SUPERVISOR_REFUSAL="no timeout supervisor (python3, GNU timeout, or gtimeout) is installed, so project commands cannot be time-limited; install one and rerun."

refuse_unsupervised() {
  [ -n "$BOOTSTRAP_REFUSAL" ] || BOOTSTRAP_REFUSAL=$NO_SUPERVISOR_REFUSAL
}

# run_if_available DESC CMD...  Queues a project-owned command for the preview.
# Nothing runs until the plan is confirmed.
run_if_available() {
  desc="$1"
  shift
  [ "$SUPERVISED" = "1" ] || refuse_unsupervised
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

# run_plan  Executes the queued commands in order, each under the bootstrap
# time limit; returns the first failing exit code (124 for a timeout).
run_plan() {
  while IFS="$(printf '\t')" read -r desc cmd; do
    info "==> $desc"
    "$SCRIPT_DIR/with-timeout.sh" "$BOOTSTRAP_TIMEOUT" "$desc" HARNESS_BOOTSTRAP_TIMEOUT sh -c "$cmd" </dev/null || {
      _status=$?
      info "FAIL: $desc (exit $_status)"
      return "$_status"
    }
  done < "$PLAN_FILE"
  return 0
}

refuse_bootstrap() {
  info "REFUSED: $BOOTSTRAP_REFUSAL"
  info "Nothing was run."
}

confirm_and_run_plan() {
  if [ -n "$BOOTSTRAP_REFUSAL" ]; then
    refuse_bootstrap
    exit 1
  fi
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
  if [ -n "$BOOTSTRAP_REFUSAL" ]; then
    refuse_bootstrap
    write_bootstrap_state failed 1 ""
    return 1
  fi
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

  if [ -n "$BOOTSTRAP_REFUSAL" ]; then
    run_bootstrap_worker > "$BOOTSTRAP_LOG" 2>&1 || :
    auto_say "target $PROJECT_ROOT registered; bootstrap REFUSED, nothing was run: $BOOTSTRAP_REFUSAL"
    return 0
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

# load_make_targets  Reads the Makefile database once. `make -qp` expands
# $(shell ...) in the Makefile, so it is project code and runs bounded; a
# probe that cannot run bounded, or does not finish, refuses the bootstrap.
MAKE_DB_STATE=""
load_make_targets() {
  [ -z "$MAKE_DB_STATE" ] || return 0
  MAKE_DB_STATE=unavailable
  if [ ! -f Makefile ] || ! has_cmd make; then
    return 0
  fi
  if [ "$SUPERVISED" != "1" ]; then
    refuse_unsupervised
    return 0
  fi
  _status=0
  "$SCRIPT_DIR/with-timeout.sh" "$BOOTSTRAP_TIMEOUT" "make -qp (Makefile target discovery)" HARNESS_BOOTSTRAP_TIMEOUT make -qp > "$MAKE_DB" 2>/dev/null </dev/null || _status=$?
  # `make -qp` exits 1 when a target is out of date; only a timeout is fatal.
  if [ "$_status" -eq 124 ]; then
    BOOTSTRAP_REFUSAL="discovering Makefile targets with make -qp did not finish within ${BOOTSTRAP_TIMEOUT}s (a \$(shell ...) in the Makefile may hang); fix the Makefile or set HARNESS_BOOTSTRAP_TIMEOUT, then rerun."
    return 0
  fi
  MAKE_DB_STATE=loaded
}

make_has_target() {
  load_make_targets
  [ "$MAKE_DB_STATE" = loaded ] && grep -q "^$1:" "$MAKE_DB"
}

# Lockfiles and the package manager that wrote each one.
NODE_LOCKFILES="pnpm-lock.yaml yarn.lock package-lock.json npm-shrinkwrap.json bun.lock bun.lockb"

lockfile_owner() {
  case "$1" in
    pnpm-lock.yaml) printf '%s\n' pnpm ;;
    yarn.lock) printf '%s\n' yarn ;;
    package-lock.json|npm-shrinkwrap.json) printf '%s\n' npm ;;
    bun.lock|bun.lockb) printf '%s\n' bun ;;
  esac
}

# detect_node_pm  Sets NODE_PM and NODE_LOCKED, or BOOTSTRAP_REFUSAL when the
# lockfile's package manager cannot be honored. Another manager would resolve a
# different dependency set than the lockfile records, so there is no fallback.
detect_node_pm() {
  NODE_PM=""
  NODE_LOCKED=0
  _locks=""
  _owner=""
  for _lock in $NODE_LOCKFILES; do
    [ -f "$_lock" ] || continue
    _pm=$(lockfile_owner "$_lock")
    _locks="${_locks:+$_locks, }$_lock"
    if [ -z "$_owner" ]; then
      _owner=$_pm
    elif [ "$_owner" != "$_pm" ]; then
      _owner=conflict
    fi
  done

  if [ "$_owner" = conflict ]; then
    BOOTSTRAP_REFUSAL="lockfiles from different package managers are present ($_locks); delete the stale ones so a single package manager owns the install, then rerun."
  elif [ -n "$_owner" ]; then
    if has_cmd "$_owner"; then
      NODE_PM=$_owner
      NODE_LOCKED=1
    else
      BOOTSTRAP_REFUSAL="$_locks was written by $_owner, but $_owner is not installed; install $_owner and rerun (installing with another package manager would give a different set of packages)."
    fi
  elif has_cmd pnpm; then
    NODE_PM=pnpm
  elif has_cmd yarn; then
    NODE_PM=yarn
  elif has_cmd npm; then
    NODE_PM=npm
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
  if [ ! -f package.json ]; then
    info "No package.json found; skipping JavaScript/TypeScript dependency install."
    return 0
  fi
  detect_node_pm
  if [ -n "$BOOTSTRAP_REFUSAL" ]; then
    return 0
  fi
  case "$NODE_PM:$NODE_LOCKED" in
    pnpm:1)
      run_if_available "installing JavaScript/TypeScript dependencies with pnpm" pnpm install --frozen-lockfile
      ;;
    pnpm:0)
      run_if_available "installing JavaScript/TypeScript dependencies with pnpm" pnpm install
      ;;
    yarn:1)
      run_if_available "installing JavaScript/TypeScript dependencies with yarn" yarn install --frozen-lockfile
      ;;
    yarn:0)
      run_if_available "installing JavaScript/TypeScript dependencies with yarn" yarn install
      ;;
    npm:1)
      run_if_available "installing JavaScript/TypeScript dependencies with npm ci" npm ci
      ;;
    npm:0)
      run_if_available "installing JavaScript/TypeScript dependencies with npm install" npm install
      ;;
    bun:1)
      run_if_available "installing JavaScript/TypeScript dependencies with bun" bun install --frozen-lockfile
      ;;
    *)
      info "package.json found, but npm/pnpm/yarn is unavailable."
      ;;
  esac
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
