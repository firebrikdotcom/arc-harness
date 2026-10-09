#!/usr/bin/env sh
# Print a git tree hash of PATH's working tree as it is on disk: tracked files
# with their uncommitted edits plus untracked files that are not ignored, less
# the top-level phase artifacts. Two equal hashes mean no project file changed
# in between. Prints "none" outside a git work tree. The real index is never
# modified.
#
#   scripts/tree-hash.sh [PATH]
set -u

ROOT=${1:-.}
if ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  printf 'none\n'
  exit 0
fi
TOP=$(git -C "$ROOT" rev-parse --show-toplevel)
INDEX=$(git -C "$TOP" rev-parse --git-path index)
case "$INDEX" in
  /*) ;;
  *) INDEX=$TOP/$INDEX ;;
esac
TMP_INDEX=$(mktemp "${TMPDIR:-/tmp}/harness-tree-index.XXXXXX")
trap 'rm -f "$TMP_INDEX"' EXIT HUP INT TERM
# Starting from a copy of the real index lets git reuse its stat cache.
if [ -f "$INDEX" ]; then
  cp "$INDEX" "$TMP_INDEX"
else
  rm -f "$TMP_INDEX"
fi
# The phase artifacts plan and review may write (progress.md, task.json,
# review-findings.json at the top) are notes about the work, not the work, so
# writing them never voids a check.
if GIT_INDEX_FILE=$TMP_INDEX git -C "$TOP" add -A -- . >/dev/null 2>&1 \
  && GIT_INDEX_FILE=$TMP_INDEX git -C "$TOP" rm -q --cached --ignore-unmatch -- progress.md task.json review-findings.json >/dev/null 2>&1 \
  && hash=$(GIT_INDEX_FILE=$TMP_INDEX git -C "$TOP" write-tree 2>/dev/null); then
  printf '%s\n' "$hash"
else
  printf 'none\n'
fi
