#!/usr/bin/env python3
"""Replace base64 images in idle Codex and Claude Code session logs with a short note.

Both agents write every image the model saw or made into their session logs as
base64 (screenshots, pasted images, generated images), which is most of a log's
size. The model never needs those bytes again from disk: a generated image is
also saved as a file (its note keeps `savedPath`), and a resumed session reads
the note instead of the picture.

Only logs that are idle (unchanged for --idle seconds) and not held open by any
process are rewritten, each by an atomic replace that is abandoned if the file
changed meanwhile. Logs last written before the first sweep are left alone
unless --all is given, so the change applies from the day it is installed.

    slim_session_logs.py [--all] [--idle SECONDS] [--dry-run] [--root DIR ...]
    slim_session_logs.py --spawn      # detached sweep, for session hooks
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
STATE = Path(os.environ.get("HARNESS_SESSION_SLIM_STATE", ROOT / ".harness-db/session-slim.json"))
DEFAULT_ROOTS = (Path.home() / ".codex/sessions", Path.home() / ".claude/projects")
MIN_IMAGE = 1024
# Lines shorter than this cannot hold an image worth removing and are copied untouched.
MIN_LINE = 8192


def note(kind: str, size: int, saved: str | None = None) -> str:
    text = f"[image removed from session log to save space: {kind}, {size:,} bytes of base64"
    return text + (f"; saved at {saved}]" if saved else "]")


def long_text(value) -> bool:
    return isinstance(value, str) and len(value) >= MIN_IMAGE


def strip(value):
    """(value with images replaced, number replaced)."""
    if isinstance(value, list):
        count = 0
        items = []
        for item in value:
            item, found = strip(item)
            items.append(item)
            count += found
        return items, count
    if isinstance(value, str):
        if value.startswith("data:image/") and len(value) >= MIN_IMAGE:
            return note(value[5:value.find(";")] if ";" in value else "image", len(value)), 1
        return value, 0
    if not isinstance(value, dict):
        return value, 0
    kind = value.get("type")
    source = value.get("source")
    # Claude Code: {"type": "image", "source": {"type": "base64", "media_type": ..., "data": ...}}
    if kind == "image" and isinstance(source, dict) and source.get("type") == "base64" and long_text(source.get("data")):
        return {"type": "text", "text": note(str(source.get("media_type", "image")), len(source["data"]))}, 1
    # MCP tool output inside Codex logs: {"type": "image", "data": ..., "mimeType": ...}
    if kind == "image" and long_text(value.get("data")):
        return {"type": "text", "text": note(str(value.get("mimeType", "image")), len(value["data"]))}, 1
    # Codex model input: {"type": "input_image", "image_url": "data:image/png;base64,..."}
    url = value.get("image_url")
    if kind == "input_image" and isinstance(url, str) and url.startswith("data:") and len(url) >= MIN_IMAGE:
        return {"type": "input_text", "text": note(url[5:url.find(";")] if ";" in url else "image", len(url))}, 1
    result = {}
    count = 0
    for key, item in value.items():
        # Codex image generation: the PNG in `result`, the file in `savedPath`.
        if key == "result" and long_text(item) and isinstance(value.get("savedPath"), str):
            result[key] = note("image/png", len(item), value["savedPath"])
            count += 1
            continue
        result[key], found = strip(item)
        count += found
    return result, count


def open_files() -> set[str] | None:
    """Paths any process holds open, or None where /proc is unavailable."""
    proc = Path("/proc")
    if not proc.is_dir():
        return None
    held = set()
    for fd_dir in proc.glob("[0-9]*/fd"):
        try:
            for fd in fd_dir.iterdir():
                try:
                    held.add(os.readlink(fd))
                except OSError:
                    pass
        except OSError:
            pass
    return held


def slim_file(path: Path, dry_run: bool) -> tuple[int, int]:
    """Rewrite one log; (images removed, bytes saved). Nothing changes if the file moved meanwhile."""
    before = path.stat()
    removed = 0
    with path.open("rb") as source, tempfile.NamedTemporaryFile("wb", dir=path.parent, prefix=".slim-", delete=False) as out:
        temp = Path(out.name)
        try:
            for line in source:
                if len(line) >= MIN_LINE:
                    try:
                        record = json.loads(line)
                    except ValueError:
                        record = None
                    if record is not None:
                        record, found = strip(record)
                        if found:
                            removed += found
                            line = json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
                out.write(line)
        except BaseException:
            temp.unlink(missing_ok=True)
            raise
    after = path.stat()
    size = temp.stat().st_size
    if dry_run or not removed or (after.st_size, after.st_mtime_ns) != (before.st_size, before.st_mtime_ns):
        temp.unlink(missing_ok=True)
        return (removed, before.st_size - size) if dry_run else (0, 0)
    os.chmod(temp, before.st_mode & 0o7777)
    os.utime(temp, ns=(before.st_atime_ns, before.st_mtime_ns))
    os.replace(temp, path)
    return removed, before.st_size - size


def sweep(roots, idle: float, everything: bool, dry_run: bool) -> dict:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    with open(STATE.with_suffix(".lock"), "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return {"skipped": "another sweep is running"}
        try:
            state = json.loads(STATE.read_text())
        except (OSError, ValueError):
            state = {}
        since = state.setdefault("since", time.time())
        done = state.setdefault("done", {})
        held = open_files()
        now = time.time()
        totals = {"files": 0, "images": 0, "bytes": 0}
        for root in roots:
            for path in sorted(Path(root).rglob("*.jsonl")) if Path(root).is_dir() else []:
                try:
                    info = path.stat()
                except OSError:
                    continue
                key = str(path)
                if done.get(key) == [info.st_size, info.st_mtime_ns]:
                    continue
                if (not everything and info.st_mtime < since) or now - info.st_mtime < idle:
                    continue
                if held is not None and key in held:
                    continue
                images, saved = slim_file(path, dry_run)
                if images:
                    totals["files"] += 1
                    totals["images"] += images
                    totals["bytes"] += saved
                if not dry_run:
                    info = path.stat()
                    done[key] = [info.st_size, info.st_mtime_ns]
        if not dry_run:
            state["done"] = {key: value for key, value in done.items() if Path(key).exists()}
            STATE.write_text(json.dumps(state))
        return totals


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--all", action="store_true", help="also slim logs last written before the first sweep")
    parser.add_argument("--idle", type=float, default=1800, help="seconds a log must be unchanged (default 1800)")
    parser.add_argument("--dry-run", action="store_true", help="report what would be removed, change nothing")
    parser.add_argument("--root", action="append", help="log directory (default: Codex and Claude Code logs)")
    parser.add_argument("--spawn", action="store_true", help="start a detached sweep and return at once")
    args = parser.parse_args()
    if args.spawn:
        # Session hooks pass their payload on stdin; the sweep needs none of it.
        subprocess.Popen([sys.executable, __file__], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)
        return 0
    totals = sweep(args.root or DEFAULT_ROOTS, args.idle, args.all, args.dry_run)
    print(json.dumps(totals))
    return 0


if __name__ == "__main__":
    sys.exit(main())
