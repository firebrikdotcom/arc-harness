#!/usr/bin/env python3
"""Dump normalized Basecamp to-dos as JSON for the dashboard prototype (GET only)."""
import datetime
import html
import json
import re
import sys
import urllib.request

TOKEN_FILE, OUT_FILE = sys.argv[1], sys.argv[2]
USER_AGENT = "Harness Task Managers prototype"


def get(url, token):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp), resp.headers


def get_all(url, token):
    items = []
    while url:
        page, headers = get(url, token)
        items.extend(page)
        url = None
        for part in (headers.get("Link") or "").split(","):
            if 'rel="next"' in part:
                url = part.split(";")[0].strip().strip("<>")
    return items


def text(rich, limit):
    plain = html.unescape(re.sub(r"<[^>]+>", " ", rich or ""))
    return re.sub(r"\s+", " ", plain).strip()[:limit]


def main():
    with open(TOKEN_FILE, encoding="utf-8") as fh:
        token = json.load(fh)["access_token"]
    auth, _ = get("https://launchpad.37signals.com/authorization.json", token)
    account = next(a for a in auth["accounts"] if a.get("product") in ("bc3", "bc4"))
    base = account["href"].rstrip("/")
    out = {
        "provider": "basecamp",
        "account": {"name": account["name"], "id": account["id"]},
        "token_expires_at": auth.get("expires_at"),
        "synced_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "projects": [],
    }
    for proj in get_all(base + "/projects.json", token):
        entry = {"id": proj["id"], "name": proj["name"], "url": proj.get("app_url"), "lists": []}
        dock = [d for d in proj.get("dock", []) if d.get("name") == "todoset" and d.get("enabled", True)]
        if dock:
            todoset, _ = get(dock[0]["url"], token)
            for lst in get_all(todoset["todolists_url"], token):
                todos = get_all(lst["todos_url"], token)
                entry["lists"].append({
                    "id": lst["id"],
                    "name": lst["name"],
                    "url": lst.get("app_url"),
                    "description": text(lst.get("description"), 300),
                    "todos": [{
                        "id": t["id"],
                        "title": text(t.get("title"), 200),
                        "description": text(t.get("description"), 4000),
                        "due_on": t.get("due_on"),
                        "assignees": [a.get("name") for a in t.get("assignees", [])],
                        "creator": (t.get("creator") or {}).get("name"),
                        "created_at": t.get("created_at"),
                        "comments_count": t.get("comments_count", 0),
                        "comments": [{
                            "author": (c.get("creator") or {}).get("name"),
                            "created_at": c.get("created_at"),
                            "text": text(c.get("content"), 1500),
                        } for c in (get_all(t["comments_url"], token) if t.get("comments_count") else [])],
                        "updated_at": t.get("updated_at"),
                        "url": t.get("app_url"),
                    } for t in todos],
                })
        out["projects"].append(entry)
    with open(OUT_FILE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    count = sum(len(l["todos"]) for p in out["projects"] for l in p["lists"])
    print(f"OK: projects={len(out['projects'])} todos={count}")


if __name__ == "__main__":
    main()
