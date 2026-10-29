#!/usr/bin/env python3
"""Remove linked Git worktrees whose work is already kept elsewhere.

    worktree_cleanup.py [--project DIR] [--dry-run] [--min-idle MINUTES]
    worktree_cleanup.py --spawn [--project DIR]      # detached run, for hooks
    worktree_cleanup.py --hook                       # SessionStart, or PostToolUse after a merge-like command

A linked worktree is removed only when all of these hold: it is not locked, has no
uncommitted or untracked files and no merge, rebase or cherry-pick in progress; its
HEAD commit is also reachable from another branch, so removing the checkout loses no
commit; its branch has commits of its own (a fresh task worktree is kept); it has been
idle for --min-idle minutes (default 60); and no process has its working directory
inside it. Removal uses `git worktree remove` without --force, then `git worktree
prune`; a branch whose worktree went is deleted only with `git branch -d`, which Git
refuses for unmerged work. Anything kept is reported with the reason.
Exit 0 on success (including nothing to remove), 2 for usage errors.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

SHELL_TOOLS = {"Bash", "shell", "exec_command", "local_shell", "container.exec"}
# Commands after which a task branch may have just been merged.
MERGE_LIKE = re.compile(r"\bgit\b[^;&|]*\b(merge|rebase|pull|cherry-pick|am)\b")


def git(cwd: str | Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True)


def worktrees(project: str) -> list[dict]:
    out = git(project, "worktree", "list", "--porcelain")
    if out.returncode != 0:
        return []
    items, item = [], {}
    for line in out.stdout.splitlines() + [""]:
        if not line:
            if item:
                items.append(item)
            item = {}
            continue
        key, _, value = line.partition(" ")
        item[key] = value or True
    return items


def busy_paths() -> list[str]:
    """Working directories of running processes, so an active worktree is kept."""
    try:
        out = subprocess.run(["lsof", "-a", "-d", "cwd", "-Fn"], capture_output=True, text=True, timeout=60)
        return [line[1:] for line in out.stdout.splitlines() if line.startswith("n")]
    except (OSError, subprocess.TimeoutExpired):
        paths = []
        for link in Path("/proc").glob("[0-9]*/cwd"):
            try:
                paths.append(os.readlink(link))
            except OSError:
                pass
        return paths


def inside(path: str, root: str) -> bool:
    return path == root or path.startswith(root.rstrip("/") + "/")


def idle_minutes(path: str) -> float:
    gitdir = git(path, "rev-parse", "--absolute-git-dir").stdout.strip()
    stamps = [os.path.getmtime(p) for p in (Path(gitdir) / "index", Path(gitdir) / "HEAD", Path(gitdir) / "logs/HEAD")
              if p.exists()]
    return (time.time() - max(stamps)) / 60 if stamps else float("inf")


def kept_elsewhere(project: str, commit: str, branch: str) -> bool:
    """True when another local or remote-tracking branch also contains the commit."""
    out = git(project, "for-each-ref", "--contains", commit, "--format=%(refname)", "refs/heads", "refs/remotes")
    others = [ref for ref in out.stdout.split() if ref != branch and not ref.endswith("/HEAD")]
    return bool(others)


def has_own_commits(project: str, branch: str, commit: str) -> bool:
    """False for a branch still at the commit it was created from (a task not started yet)."""
    if not branch:
        return True
    log = git(project, "reflog", "show", "--format=%H", branch.removeprefix("refs/heads/"))
    created = log.stdout.split()[-1:] if log.returncode == 0 else []
    return not created or created[0] != commit


def reason_to_keep(project: str, tree: dict, busy: list[str], min_idle: float) -> str | None:
    path = tree["worktree"]
    if "locked" in tree:
        return "locked"
    if not os.path.isdir(path):
        return None  # already gone; prune clears it
    if git(path, "status", "--porcelain", "--untracked-files=all").stdout.strip():
        return "uncommitted or untracked files"
    gitdir = Path(git(path, "rev-parse", "--absolute-git-dir").stdout.strip())
    if any((gitdir / name).exists() for name in ("MERGE_HEAD", "rebase-merge", "rebase-apply", "CHERRY_PICK_HEAD")):
        return "merge or rebase in progress"
    commit, branch = tree.get("HEAD", ""), tree.get("branch", "")
    if not kept_elsewhere(project, commit, branch if isinstance(branch, str) else ""):
        return "commits not on any other branch"
    if not has_own_commits(project, branch if isinstance(branch, str) else "", commit):
        return "no commits of its own yet"
    if idle_minutes(path) < min_idle:
        return f"used in the last {int(min_idle)} minutes"
    if any(inside(p, path) for p in busy):
        return "a process is using it"
    return None


def clean(project: str, dry_run: bool, min_idle: float) -> list[str]:
    trees = worktrees(project)
    if not trees:
        return []
    main_path = trees[0]["worktree"]
    busy = busy_paths()
    report = []
    for tree in trees[1:]:
        path, branch = tree["worktree"], tree.get("branch", "")
        why = reason_to_keep(main_path, tree, busy, min_idle)
        if why:
            report.append(f"kept: {path} ({why})")
            continue
        if dry_run:
            report.append(f"would remove: {path}")
            continue
        if os.path.isdir(path):
            done = git(main_path, "worktree", "remove", path)
            if done.returncode != 0:
                report.append(f"kept: {path} ({done.stderr.strip().splitlines()[-1:] or ['remove failed']})")
                continue
        report.append(f"removed: {path}")
        if isinstance(branch, str) and branch.startswith("refs/heads/"):
            name = branch.removeprefix("refs/heads/")
            if git(main_path, "branch", "-d", name).returncode == 0:
                report.append(f"deleted branch: {name}")
    if not dry_run:
        git(main_path, "worktree", "prune")
    return report


def spawn(project: str) -> None:
    subprocess.Popen([sys.executable, __file__, "--project", project], stdin=subprocess.DEVNULL,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)


def hook() -> int:
    """SessionStart, or PostToolUse after a merge-like shell command: start a detached cleanup.
    Never blocks or fails the call."""
    try:
        payload = json.loads(sys.stdin.read(1_000_000) or "{}")
    except ValueError:
        return 0
    if payload.get("hook_event_name") == "SessionStart":
        if os.path.isdir(payload.get("cwd") or ""):
            spawn(payload["cwd"])
        return 0
    if payload.get("tool_name") not in SHELL_TOOLS:
        return 0
    tool_input = payload.get("tool_input") if isinstance(payload.get("tool_input"), dict) else {}
    command = tool_input.get("command", tool_input.get("cmd", ""))
    command = " ".join(map(str, command)) if isinstance(command, list) else str(command)
    cwd = tool_input.get("workdir") or payload.get("cwd") or ""
    if MERGE_LIKE.search(command) and os.path.isdir(cwd):
        spawn(cwd)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--project", default=os.getcwd())
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--min-idle", type=float, default=60.0)
    parser.add_argument("--spawn", action="store_true", help="start a detached cleanup and return at once")
    parser.add_argument("--hook", action="store_true", help="PostToolUse hook mode (reads the payload on stdin)")
    args = parser.parse_args()
    if args.hook:
        return hook()
    if not os.path.isdir(args.project):
        print(f"FAIL: no such directory: {args.project}", file=sys.stderr)
        return 2
    if args.spawn:
        spawn(args.project)
        return 0
    for line in clean(args.project, args.dry_run, args.min_idle):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
