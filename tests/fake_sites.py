"""Minimal Firebrik Sites stand-in for tests/sites.sh.

  python3 tests/fake_sites.py PORT_FILE MODE

MODE: healthy | old (no /api/pages upload endpoint) | broken (every call 503) |
mangle (stores and reports a changed index.html) | readonly (token lacks
pages:write). The valid token is "good-token". Pages are kept in memory and
served at /testuser/<route> and /testuser/<route>/<asset>.
"""
import hashlib
import io
import json
import sys
import zipfile
from email.parser import BytesParser
from email.policy import HTTP
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import PurePosixPath

PORT_FILE, MODE = sys.argv[1], sys.argv[2]
pages = {}  # route -> {"id": int, "files": {path: bytes}}


def entry(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def unzip(data):
    files = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for info in archive.infolist():
            base = PurePosixPath(info.filename).name
            if info.filename.endswith("/") or (base.startswith(".") and "." not in base[1:]):
                continue
            files[info.filename] = archive.read(info)
    tops = {name.split("/", 1)[0] for name in files}
    if files and len(tops) == 1 and all("/" in name for name in files):
        prefix = next(iter(tops)) + "/"
        files = {name[len(prefix):]: value for name, value in files.items()}
    return files


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, status, body, kind="application/json"):
        data = body if isinstance(body, bytes) else (body.encode() if isinstance(body, str) else json.dumps(body).encode())
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        parts = self.path.strip("/").split("/", 2)
        page = pages.get(parts[1]) if len(parts) >= 2 else None
        if not page:
            return self.send(404, "not found", "text/plain")
        if len(parts) == 2:
            return self.send(200, b"<html><head><base href='/'></head>" + page["files"]["index.html"], "text/html")
        asset = page["files"].get(parts[2])
        return self.send(200, asset, "application/octet-stream") if asset is not None else self.send(404, "not found", "text/plain")

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if MODE == "broken":
            return self.send(503, "<html>Scheduled Maintenance</html>", "text/html")
        if self.path == "/api/pages" and MODE == "old":
            return self.send(404, "<html>Not Found</html>", "text/html")
        auth = self.headers.get("Authorization")
        if not auth:
            return self.send(401, {"error": "Missing API token"})
        if auth != "Bearer good-token":
            return self.send(401, {"error": "Invalid or expired API token"})
        if self.path == "/api/pages":
            return self.upload(body)
        request = json.loads(body or b"{}")
        params = request.get("params") or {}
        if params.get("name") == "list-pages":
            listed = [{"id": p["id"], "route_path": r, "url": f"https://prod.invalid/testuser/{r}"} for r, p in pages.items()]
            return self.send(200, {"jsonrpc": "2.0", "id": 1, "result": {"structuredContent": {"pages": listed, "last_page": 1}}})
        return self.send(200, {"jsonrpc": "2.0", "id": 1, "result": {"isError": True, "content": [{"type": "text", "text": "unknown"}]}})

    def upload(self, body):
        if MODE == "readonly":
            return self.send(403, {"error": "This API token may not publish pages (needs the pages:write ability)."})
        kind = self.headers.get("Content-Type") or ""
        message = BytesParser(policy=HTTP).parsebytes(b"Content-Type: " + kind.encode() + b"\r\n\r\n" + body)
        fields, upload, filename = {}, None, ""
        for part in (message.iter_parts() if kind.startswith("multipart/") else []):
            name = part.get_param("name", header="content-disposition")
            if name == "file":
                upload, filename = part.get_payload(decode=True), part.get_filename() or ""
            else:
                fields[name] = part.get_content().strip()
        if upload is None or not fields.get("route_path"):
            return self.send(422, {"message": "The file field is required.", "errors": {"file": ["required"]}})
        received = hashlib.sha256(upload).hexdigest()
        if fields.get("sha256") and fields["sha256"] != received:
            return self.send(422, {"error": "sha256 mismatch. Nothing was written."})
        is_zip = zipfile.is_zipfile(io.BytesIO(upload))
        files = unzip(upload) if is_zip else {"index.html": upload}
        if "index.html" not in files:
            return self.send(422, {"error": "The zip file must contain an index.html at the root level."})
        if MODE == "mangle":
            files["index.html"] = files["index.html"][:-10]
        route = fields["route_path"]
        created = route not in pages
        page = pages.setdefault(route, {"id": len(pages) + 1, "files": {}})
        page["files"] = files if is_zip else {**page["files"], **files}
        index = page["files"]["index.html"].decode()
        return self.send(201 if created else 200, {
            "id": page["id"], "route_path": route, "url": f"https://prod.invalid/testuser/{route}",
            "action": "created" if created else "updated", "kind": "zip" if is_zip else "html",
            "received_bytes": len(upload), "received_sha256": received,
            "files": [{"path": p, **entry(d)} for p, d in sorted(page["files"].items())],
            "verified": True, "tail_excerpt": index[-120:], "warnings": [],
        })


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w") as handle:
    handle.write(str(server.server_address[1]))
server.serve_forever()
