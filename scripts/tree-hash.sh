#!/usr/bin/env sh
# Print a hash of PATH's project files as they are on disk. In a git work tree it
# is a git tree: tracked files with their uncommitted edits plus untracked files
# that are not ignored. Outside git it is a content hash of every file (version
# control and dependency directories skipped). Either way the phase notes at the
# project root (progress.md, tasks/, task.json, review-findings.json; PATH may be
# a subdirectory of a larger repository) are left out: they
# are notes about the work, not the work, so writing them never voids a check.
# Two equal hashes mean no project file changed in between. The real index is
# never modified.
#
#   scripts/tree-hash.sh [PATH]
set -u

ROOT=${1:-.}
if ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [ -d "$ROOT" ] && command -v python3 >/dev/null 2>&1; then
    python3 - "$ROOT" <<'PY'
import hashlib, os, sys
root = sys.argv[1]
skip_dirs = {".git", ".hg", ".svn", "node_modules", ".venv", "vendor", "__pycache__", ".harness-db"}
notes = {"progress.md", "task.json", "review-findings.json"}
digest = hashlib.sha256()
for base, dirs, files in os.walk(root):
    dirs[:] = sorted(d for d in dirs if d not in skip_dirs and not (base == root and d == "tasks"))
    for name in sorted(files):
        if base == root and name in notes:
            continue
        path = os.path.join(base, name)
        digest.update(os.path.relpath(path, root).encode() + b"\0")
        try:
            with open(path, "rb") as handle:
                for chunk in iter(lambda: handle.read(1 << 20), b""):
                    digest.update(chunk)
        except OSError:
            digest.update(b"<unreadable>")
print("files-" + digest.hexdigest())
PY
    exit 0
  fi
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
if GIT_INDEX_FILE=$TMP_INDEX git -C "$TOP" add -A -- . >/dev/null 2>&1 \
  && GIT_INDEX_FILE=$TMP_INDEX git -C "$ROOT" rm -r -q --cached --ignore-unmatch -- progress.md tasks task.json review-findings.json >/dev/null 2>&1 \
  && hash=$(GIT_INDEX_FILE=$TMP_INDEX git -C "$TOP" write-tree 2>/dev/null); then
  printf '%s\n' "$hash"
else
  printf 'none\n'
fi
