#!/usr/bin/env sh
set -eu

# The one list of tools a project's verification needs, shared by the local
# bootstrap and CI so the two cannot drift apart. The list is
# .harness-required-tools at the project root, one `TOOL [APT_PACKAGE]` per
# line; `#` starts a comment and a package of `-` means the environment
# already provides the tool. `git` and `sh` are required for every project,
# listed or not, because the harness itself runs on them.
#
#   check [--quiet]  exit 1 naming each missing tool (scripts/init.sh runs this)
#   packages         print the listed apt packages, one per line (CI installs these)
#
# Only shell builtins are used, so `check` still works on a PATH that lacks
# the very tools it reports. It never installs anything.

BASELINE_TOOLS="git sh"
LIST_NAME=.harness-required-tools
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"
QUIET=0

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/required-tools.sh check [--quiet] [--project PATH]"
  info "       scripts/required-tools.sh packages [--project PATH]"
  info ""
  info "Reads $LIST_NAME at the project root (default: the current directory)."
  info "check exits 1 when a required tool is missing and 2 when the list is malformed."
}

# valid_name WORD  A tool or package name: letters, digits, and . _ + -, not
# starting with - (so a package can never be read as an apt-get option).
valid_name() {
  case "$1" in
    ''|-*|*[!A-Za-z0-9._+-]*) return 1 ;;
  esac
  return 0
}

if [ "$#" -eq 0 ]; then
  usage
  exit 2
fi
MODE=$1
shift
case "$MODE" in
  check|packages) ;;
  --help|-h)
    usage
    exit 0
    ;;
  *)
    info "FAIL: unknown command: $MODE"
    usage
    exit 2
    ;;
esac

while [ "$#" -gt 0 ]; do
  case "$1" in
    --project)
      if [ "$#" -lt 2 ]; then
        info "FAIL: --project requires a path."
        exit 2
      fi
      PROJECT_ROOT=$2
      shift 2
      ;;
    --quiet)
      [ "$MODE" = check ] || { info "FAIL: --quiet applies only to check."; exit 2; }
      QUIET=1
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
LIST_FILE=$PROJECT_ROOT/$LIST_NAME

# Parsed entries, newline-separated `TOOL PACKAGE` with PACKAGE `-` when absent.
entries=""
if [ -f "$LIST_FILE" ]; then
  set -f
  lineno=0
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    line=${line%%#*}
    # shellcheck disable=SC2086 # split the line into its fields; globbing is off
    set -- $line
    [ "$#" -gt 0 ] || continue
    if [ "$#" -gt 2 ]; then
      info "FAIL: $LIST_NAME:$lineno: expected TOOL [APT_PACKAGE], got $# fields."
      exit 2
    fi
    tool=$1
    package=${2:--}
    if ! valid_name "$tool"; then
      info "FAIL: $LIST_NAME:$lineno: invalid tool name: $tool"
      exit 2
    fi
    if [ "$package" != "-" ] && ! valid_name "$package"; then
      info "FAIL: $LIST_NAME:$lineno: invalid package name: $package"
      exit 2
    fi
    entries="$entries$tool $package
"
  done < "$LIST_FILE"
  set +f
fi

if [ "$MODE" = packages ]; then
  seen=" "
  printf '%s' "$entries" | while read -r tool package; do
    [ "$package" != "-" ] || continue
    case "$seen" in *" $package "*) continue ;; esac
    seen="$seen$package "
    info "$package"
  done
  exit 0
fi

missing=0
seen=" "
# check_tool TOOL PACKAGE
check_tool() {
  case "$seen" in *" $1 "*) return 0 ;; esac
  seen="$seen$1 "
  if command -v "$1" >/dev/null 2>&1; then
    [ "$QUIET" = 1 ] || info "found: $1"
  elif [ "$2" = "-" ]; then
    info "missing required tool: $1"
    missing=1
  else
    info "missing required tool: $1 (apt package: $2)"
    missing=1
  fi
}

for tool in $BASELINE_TOOLS; do
  package=-
  # A baseline tool that is also listed reports the listed package.
  while read -r listed listed_package; do
    if [ "$listed" = "$tool" ]; then
      package=$listed_package
    fi
  done <<EOF
$entries
EOF
  check_tool "$tool" "$package"
done
while read -r tool package; do
  [ -n "$tool" ] || continue
  check_tool "$tool" "$package"
done <<EOF
$entries
EOF

exit "$missing"
