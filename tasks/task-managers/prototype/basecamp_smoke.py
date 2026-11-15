#!/usr/bin/env python3
"""Throwaway Basecamp smoke test: OAuth login, then read-only listing.

Reads app credentials from an env file, saves tokens beside it (0600), and
prints names and counts only. After the token exchange it sends GET only.
"""
import http.server
import json
import os
import secrets
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

ENV_FILE = sys.argv[1]
TOKEN_FILE = os.path.join(os.path.dirname(ENV_FILE), "basecamp.oauth")
WANT_PROJECT = sys.argv[2] if len(sys.argv) > 2 else None
LAUNCHPAD = "https://launchpad.37signals.com"
USER_AGENT = "Harness Task Managers smoke test"


def load_env(path):
    out = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            out[key.strip()] = value.strip().strip('"').strip("'")
    return out


def request(url, token=None, method="GET"):
    headers = {"User-Agent": USER_AGENT}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(url, method=method, headers=headers, data=b"" if method == "POST" else None)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.load(resp), resp.headers
    except urllib.error.HTTPError as err:
        body = err.read(300).decode("utf-8", "replace")
        sys.exit(f"FAIL: {method} {url.split('?')[0]} -> HTTP {err.code}: {body}")


def get_all(url, token):
    items = []
    while url:
        page, headers = request(url, token)
        items.extend(page)
        url = None
        for part in (headers.get("Link") or "").split(","):
            if 'rel="next"' in part:
                url = part.split(";")[0].strip().strip("<>")
    return items


def wait_for_code(redirect_uri, state):
    parsed = urllib.parse.urlparse(redirect_uri)
    result = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            got = urllib.parse.urlparse(self.path)
            query = urllib.parse.parse_qs(got.query)
            if got.path != parsed.path:
                self.send_response(404)
                self.end_headers()
                return
            result.update({k: v[0] for k, v in query.items()})
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"Basecamp login received. You can close this tab.")

        def log_message(self, *args):
            pass

    server = http.server.HTTPServer(("127.0.0.1", parsed.port), Handler)
    server.timeout = int(os.environ.get("WAIT_SECONDS", "420"))
    while "code" not in result and "error" not in result:
        server.handle_request()
        if not result:
            sys.exit("FAIL: timed out waiting for the browser redirect")
    server.server_close()
    if "error" in result:
        sys.exit(f"FAIL: authorization returned error={result['error']}")
    if result.get("state") != state:
        sys.exit("FAIL: state mismatch on redirect")
    return result["code"]


def login(env):
    state = secrets.token_urlsafe(16)
    common = {"type": "web_server", "client_id": env["BASECAMP_CLIENT_ID"], "redirect_uri": env["BASECAMP_REDIRECT_URI"]}
    auth_url = LAUNCHPAD + "/authorization/new?" + urllib.parse.urlencode({**common, "state": state})
    url_file = os.environ.get("AUTH_URL_FILE")
    if url_file:
        with open(url_file, "w", encoding="utf-8") as fh:
            fh.write(auth_url)
        print("Waiting for Basecamp approval (URL written for the opener).", flush=True)
    else:
        print("Opening browser for Basecamp approval. If it does not open, visit:\n" + auth_url, flush=True)
        subprocess.Popen(["xdg-open", auth_url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    code = wait_for_code(env["BASECAMP_REDIRECT_URI"], state)
    token_url = LAUNCHPAD + "/authorization/token?" + urllib.parse.urlencode(
        {**common, "client_secret": env["BASECAMP_CLIENT_SECRET"], "code": code}
    )
    tokens, _ = request(token_url, method="POST")
    fd = os.open(TOKEN_FILE, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(tokens, fh)
    print(f"OK: token exchange succeeded; fields={sorted(tokens)} expires_in={tokens.get('expires_in')}")
    return tokens


def main():
    env = load_env(ENV_FILE)
    if os.path.exists(TOKEN_FILE):
        with open(TOKEN_FILE, encoding="utf-8") as fh:
            tokens = json.load(fh)
        print("Using saved tokens.")
    else:
        tokens = login(env)
    token = tokens["access_token"]

    auth, _ = request(LAUNCHPAD + "/authorization.json", token)
    accounts = auth.get("accounts", [])
    print(f"OK: authorization.json; expires_at={auth.get('expires_at')}; accounts={len(accounts)}")
    for acct in accounts:
        print(f"  account product={acct.get('product')} name={acct.get('name')!r} id={acct.get('id')}")
    bc = [a for a in accounts if a.get("product") in ("bc3", "bc4")]
    if not bc:
        sys.exit("FAIL: no Basecamp 3/4 account on this identity")
    base = bc[0]["href"].rstrip("/")

    projects = get_all(base + "/projects.json", token)
    print(f"OK: projects={len(projects)}")
    for proj in projects[:30]:
        print(f"  project {proj['name']!r} id={proj['id']}")
    if not projects:
        return
    project = projects[0]
    if WANT_PROJECT:
        match = [p for p in projects if WANT_PROJECT.lower() in p["name"].lower()]
        if not match:
            sys.exit(f"FAIL: no project matching {WANT_PROJECT!r}")
        project = match[0]

    dock = [d for d in project.get("dock", []) if d.get("name") == "todoset"]
    if not dock or not dock[0].get("enabled", True):
        print(f"NOTE: project {project['name']!r} has no enabled to-do set")
        return
    todoset, _ = request(dock[0]["url"], token)
    lists = get_all(todoset["todolists_url"], token)
    print(f"OK: project {project['name']!r} to-do lists={len(lists)}")
    for lst in lists[:10]:
        todos = get_all(lst["todos_url"], token)
        print(f"  list {lst['name']!r}: open to-dos={len(todos)}")
        for todo in todos[:5]:
            print(f"    - {todo.get('title', '')[:80]!r} due={todo.get('due_on')} assignees={len(todo.get('assignees', []))}")


if __name__ == "__main__":
    main()
