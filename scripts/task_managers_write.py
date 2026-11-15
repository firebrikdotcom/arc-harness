#!/usr/bin/env python3
"""Task managers: write back to the provider. One request per write.

  task_managers.py comment --item ID --text-file F   -> {"comment_id", "url"}
  task_managers.py complete --item ID                -> {"status": "done"}
  task_managers.py list-create --project ID --name-file F [--description-file F]
                                                     -> {"list": {"id", "name", "url"}}
  task_managers.py todo-create --list ID --title-file F [--description-file F] [--due DATE]
                                                     -> {"item"}
  task_managers.py todo-update --item ID [--title-file F] [--description-file F]
                   [--due DATE | --clear-due] [--reopen] -> {"item"}

Text is read from files, never from the command line. A failed write is
reported (exit 5) and never sent again here; the only repeat is the one
ctx.http makes itself after refreshing a refused token. After a write the
cached answers of the project are dropped, so the next read shows it.
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
MAX_NAME_CHARS = tm.TITLE_LIMIT  # a list name or a to-do title, so a read shows it whole
MAX_DESCRIPTION_CHARS = 60_000
# Fields the to-do PUT must carry: Basecamp clears every one the body leaves out.
TODO_FIELDS = ("content", "description", "assignees", "completion_subscribers", "due_on", "starts_on")


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


def rich_text(text: Any, what: str = "comment text", limit: int = MAX_TEXT_CHARS, empty: bool = False) -> str:
    """Plain text as the HTML Basecamp stores: escaped, line breaks as <br>. Raises UsageError
    unless the text, without surrounding white space, has 1 to `limit` characters (0 with `empty`)."""
    if not isinstance(text, str):
        raise tm.UsageError(f"the {what} must be text", "text")
    text = text.replace("\r\n", "\n").replace("\r", "\n").strip()
    if not text and not empty:
        raise tm.UsageError(f"the {what} is empty", "text")
    if len(text) > limit:
        raise tm.UsageError(f"the {what} exceeds {limit} characters", "text")
    if any(ord(c) < 32 and c not in "\n\t" for c in text):
        raise tm.UsageError(f"the {what} holds a control character", "text")
    return html.escape(text, quote=True).replace("\n", "<br>")


def comment_content(text: Any) -> str:
    return rich_text(text)


def one_line(text: Any, what: str) -> str:
    """A list name or a to-do title: one line of 1 to 200 characters, sent as plain text."""
    if not isinstance(text, str):
        raise tm.UsageError(f"the {what} must be text", "text")
    text = text.strip()
    if not text:
        raise tm.UsageError(f"the {what} is empty", "text")
    if len(text) > MAX_NAME_CHARS:
        raise tm.UsageError(f"the {what} exceeds {MAX_NAME_CHARS} characters", "text")
    if any(ord(c) < 32 or ord(c) == 127 for c in text):
        raise tm.UsageError(f"the {what} must be one line without control characters", "text")
    return text


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
    return _write(ctx, "POST", item["_bucket"], path, data, accept)


def _write(ctx, method: str, bucket: str, path: str, data: Any, accept: tuple[int, ...]) -> bytes:
    """Exactly one write under the project; any answer outside `accept` is an error, never a retry."""
    cfg = ctx.provider_cfg()
    bucket = tm._digits(bucket)
    url = f"{cfg['api_base']}/{cfg['account_id']}/buckets/{bucket}/{path}"
    try:
        status, _, body = ctx.http(method, url, data=data)
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


def _answer(body: bytes) -> dict[str, Any] | None:
    """The provider's JSON object after a confirmed write, or None when it cannot be read: the
    record exists then, and reporting a failure would invite the caller to send it twice."""
    try:
        made = json.loads(body.decode("utf-8"))
    except (UnicodeError, ValueError):
        return None
    return made if isinstance(made, dict) else None


def _shown(ctx, todo: dict[str, Any] | None, place: dict[str, Any]) -> dict[str, Any] | None:
    """A to-do from a write answer as a normalized item (full description), or None."""
    if todo is None:
        return None
    try:
        return tm.public(ctx.reader()._item(todo, place["list"], place["project"], True))
    except (tm.TaskManagerError, KeyError, TypeError, AttributeError):
        return None


def list_create(ctx, project_id: str, name: str, description: str | None = None) -> dict[str, Any]:
    """Create a to-do list in the project's to-do set. Text is checked before any request."""
    body = {"name": one_line(name, "list name")}
    if description is not None and description.strip():
        body["description"] = rich_text(description, "description", MAX_DESCRIPTION_CHARS)
    todoset = ctx.reader().todoset_id(str(project_id))
    made = _answer(_write(ctx, "POST", project_id, f"todosets/{todoset}/todolists.json", body, (201,)))
    try:
        return {"list": {**tm._ref(made), "url": tm._link(made.get("app_url"))}}
    except (tm.TaskManagerError, AttributeError, TypeError):
        return {"list": {"id": None, "name": None, "url": None}}


def todo_create(ctx, list_id: str, title: str, description: str | None = None, due: str | None = None) -> dict[str, Any]:
    """Create a to-do at the end of the list."""
    body = {"content": one_line(title, "title")}
    if description is not None and description.strip():
        body["description"] = rich_text(description, "description", MAX_DESCRIPTION_CHARS)
    if due:
        body["due_on"] = due
    place = ctx.reader().locate_list(str(list_id))
    made = _answer(_write(ctx, "POST", place["_bucket"], f"todolists/{place['list']['id']}/todos.json", body, (201,)))
    return {"item": _shown(ctx, made, place)}


def _ids(people: Any) -> list[int]:
    if not isinstance(people, list):
        raise tm.Upstream("the provider's to-do lists people in an unexpected shape; nothing was changed", "unexpected_response")
    out = []
    for person in people:
        if not isinstance(person, dict) or type(person.get("id")) is not int:
            raise tm.Upstream("the provider's to-do lists a person without an id; nothing was changed", "unexpected_response")
        out.append(person["id"])
    return out


def todo_update(ctx, item_id: str, title: str | None = None, description: str | None = None,
                due: str | None = None, clear_due: bool = False, reopen: bool = False) -> dict[str, Any]:
    """Change the given fields of a to-do and keep every other one. Basecamp's PUT clears what
    its body leaves out, so the item is read fresh (never from the cache) and each field that
    is not changed is sent back as it is. --reopen sends one DELETE of the completion, after
    the PUT, and only when the item is done."""
    changes: dict[str, Any] = {}
    if title is not None:
        changes["content"] = one_line(title, "title")
    if description is not None:
        changes["description"] = rich_text(description, "description", MAX_DESCRIPTION_CHARS, empty=True)
    if due:
        changes["due_on"] = due
    elif clear_due:
        changes["due_on"] = None
    if not changes and not reopen:
        raise tm.UsageError("nothing to change: give a title, description, due date or --reopen", "nothing_to_change")
    located = ctx.find_item(str(item_id))
    bucket, place = located["_bucket"], {"project": located["project"], "list": located["list"]}
    cfg = ctx.provider_cfg()
    status, _, raw = ctx.http("GET", f"{cfg['api_base']}/{cfg['account_id']}/buckets/{bucket}/todos/{located['id']}.json")
    if status == 404:
        raise tm.NotFound(f"no item {located['id']}")
    if status in (401, 403):
        raise tm.Upstream(f"the provider refused the token (HTTP {status}); run the login command", "unauthorized")
    current = _answer(raw) if status == 200 else None
    if current is None or any(key not in current for key in TODO_FIELDS):
        raise tm.Upstream(f"cannot read the to-do before changing it (HTTP {status}); nothing was changed", "unexpected_response")
    todo = current
    if changes:
        body = {"content": current["content"], "description": current["description"] or "",
                "assignee_ids": _ids(current["assignees"]), "completion_subscriber_ids": _ids(current["completion_subscribers"]),
                "due_on": current["due_on"], "starts_on": current["starts_on"], **changes}
        todo = _answer(_write(ctx, "PUT", bucket, f"todos/{located['id']}.json", body, (200,)))
    if reopen and current.get("completed") is True:
        _write(ctx, "DELETE", bucket, f"todos/{located['id']}/completion.json", None, (200, 204))
        if todo is not None:
            todo = {**todo, "completed": False}
    return {"item": _shown(ctx, todo, place)}


def _optional_text(file: Path | None) -> str | None:
    return None if file is None else read_text_file(file)


def main(args, ctx) -> int:
    if args.cmd == "comment":
        ctx.emit(comment(ctx, args.item, read_text_file(args.text_file)))
    elif args.cmd == "complete":
        ctx.emit(complete(ctx, args.item))
    elif args.cmd == "list-create":
        ctx.emit(list_create(ctx, args.project, read_text_file(args.name_file), _optional_text(args.description_file)))
    elif args.cmd == "todo-create":
        ctx.emit(todo_create(ctx, args.list, read_text_file(args.title_file), _optional_text(args.description_file), args.due))
    elif args.cmd == "todo-update":
        ctx.emit(todo_update(ctx, args.item, _optional_text(args.title_file), _optional_text(args.description_file),
                             args.due, args.clear_due, args.reopen))
    else:
        raise tm.UsageError(f"{args.cmd} is not a write command")
    return 0
