#!/usr/bin/env python3
"""Task managers: write back to the provider. Two actions, one POST each.

  task_managers.py comment --item ID --text-file F   -> {"comment_id", "url"}
  task_managers.py complete --item ID                -> {"status": "done"}

The comment text is read from a file, never from the command line. A failed
POST is reported (exit 5) and never sent again here; the only repeat is the
one ctx.http makes itself after refreshing a refused token. After a write the
cached answers of the item's project are dropped, so the next read shows it.
"""
from __future__ import annotations

import hashlib
import html
import json
import os
import stat
import urllib.parse
from pathlib import Path
from typing import Any

import task_managers as tm

MAX_TEXT_CHARS = 10_000
MAX_TEXT_FILE_BYTES = 64 * 1024


def read_text_file(file: Path | str) -> str:
    """The comment text of a regular file of at most 64 KiB; raises UsageError."""
    try:
        fd = os.open(file, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_CLOEXEC", 0))
    except (OSError, ValueError) as error:
        raise tm.UsageError(f"cannot open the text file: {type(error).__name__}", "text_file") from None
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise tm.UsageError("the text file must be a regular file", "text_file")
        if info.st_size > MAX_TEXT_FILE_BYTES:
            raise tm.UsageError(f"the text file exceeds {MAX_TEXT_FILE_BYTES} bytes", "text_file")
        with os.fdopen(fd, "rb") as stream:
            fd = None
            data = stream.read(MAX_TEXT_FILE_BYTES + 1)
    except OSError as error:
        raise tm.UsageError(f"cannot read the text file: {type(error).__name__}", "text_file") from None
    finally:
        if fd is not None:
            os.close(fd)
    if len(data) > MAX_TEXT_FILE_BYTES:
        raise tm.UsageError(f"the text file exceeds {MAX_TEXT_FILE_BYTES} bytes", "text_file")
    try:
        return data.decode("utf-8")
    except UnicodeError:
        raise tm.UsageError("the text file is not UTF-8", "text_file") from None


def comment_content(text: Any) -> str:
    """Plain text as the HTML Basecamp stores: escaped, line breaks as <br>. Raises UsageError
    unless the text, without surrounding white space, has 1 to 10000 characters."""
    if not isinstance(text, str):
        raise tm.UsageError("the comment text must be text", "text")
    text = text.replace("\r\n", "\n").replace("\r", "\n").strip()
    if not text:
        raise tm.UsageError("the comment text is empty", "text")
    if len(text) > MAX_TEXT_CHARS:
        raise tm.UsageError(f"the comment text exceeds {MAX_TEXT_CHARS} characters", "text")
    if any(ord(c) < 32 and c not in "\n\t" for c in text):
        raise tm.UsageError("the comment text holds a control character", "text")
    return html.escape(text, quote=True).replace("\n", "<br>")


def forget(ctx, bucket: str) -> int:
    """Drop the cached answers under the item's project (its lists, to-dos, comments),
    so the next read asks the provider again. Returns how many entries went."""
    cfg = ctx.provider_cfg()
    prefix = f"{cfg['api_base']}/{cfg['account_id']}/buckets/{bucket}/"
    gone = 0
    try:
        files = sorted(ctx.cache_dir().glob("*.json"))
    except OSError:
        return 0
    for file in files:
        try:
            url = json.loads(file.read_text(encoding="utf-8")).get("url")
            # Only a file the reader wrote: named after the URL it holds.
            if (isinstance(url, str) and url.startswith(prefix)
                    and file.name == hashlib.sha256(url.encode("utf-8")).hexdigest()[:32] + ".json"):
                file.unlink()
                gone += 1
        except (OSError, UnicodeError, ValueError, AttributeError):
            continue
    return gone


def _post(ctx, item: dict[str, Any], path: str, data: Any, accept: tuple[int, ...]) -> bytes:
    """Exactly one POST for the item; any answer outside `accept` is an error, never a retry."""
    cfg = ctx.provider_cfg()
    bucket = tm._digits(item["_bucket"])
    url = f"{cfg['api_base']}/{cfg['account_id']}/buckets/{bucket}/{path}"
    try:
        status, _, body = ctx.http("POST", url, data=data)
    finally:
        # Also after a failure: the provider may have applied a write it did not confirm.
        forget(ctx, bucket)
    if status in accept:
        return body
    if status == 404:
        raise tm.NotFound("the provider has no such record (HTTP 404)")
    if status in (401, 403):
        raise tm.Upstream(f"the provider refused the write (HTTP {status}); run the login command", "unauthorized")
    raise tm.Upstream(f"the provider answered HTTP {status}; the write was not repeated", "rate_limited" if status == 429 else "upstream")


def comment(ctx, item_id: str, text: str) -> dict[str, Any]:
    """Post `text` as a comment on the item. The text is checked before any request."""
    content = comment_content(text)
    item = ctx.find_item(str(item_id))
    body = _post(ctx, item, f"recordings/{urllib.parse.quote(item['id'], safe='')}/comments.json", {"content": content}, (201,))
    # The comment exists once the provider said 201; an unreadable answer must not
    # read as a failure, or the caller would post it again.
    comment_id = url = None
    try:
        made = json.loads(body.decode("utf-8"))
        if isinstance(made, dict):
            comment_id, url = tm._digits(made.get("id")), tm._link(made.get("app_url"))
    except (UnicodeError, ValueError, tm.TaskManagerError):
        pass
    return {"comment_id": comment_id, "url": url}


def complete(ctx, item_id: str) -> dict[str, Any]:
    """Mark the item done."""
    item = ctx.find_item(str(item_id))
    _post(ctx, item, f"todos/{urllib.parse.quote(item['id'], safe='')}/completion.json", None, (201, 204))
    return {"status": "done"}


def main(args, ctx) -> int:
    if args.cmd == "comment":
        ctx.emit(comment(ctx, args.item, read_text_file(args.text_file)))
    elif args.cmd == "complete":
        ctx.emit(complete(ctx, args.item))
    else:
        raise tm.UsageError(f"{args.cmd} is not a write command")
    return 0
