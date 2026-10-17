"""Minimal Firebrik Sites MCP stand-in for tests/sites.sh.

  python3 tests/fake_sites.py PORT_FILE MODE

MODE: healthy | old (no integrity support) | broken (every call 500) |
mangle (stores and reports different bytes than it received).
The valid token is "good-token". Pages are kept in memory and served at
/testuser/<route>.
"""
import hashlib
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT_FILE, MODE = sys.argv[1], sys.argv[2]
pages = {}


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, status, body, kind="application/json"):
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        route = self.path.rsplit("/", 1)[-1]
        page = next((p for p in pages.values() if p["route_path"] == route), None)
        if page:
            self.send(200, "<html><head><base href='/'></head>" + page["html"], "text/html")
        else:
            self.send(404, "not found", "text/plain")

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])) or b"{}")
        if MODE == "broken":
            return self.send(503, "<html>Scheduled Maintenance</html>", "text/html")
        auth = self.headers.get("Authorization")
        if not auth:
            return self.send(401, {"error": "Missing API token"})
        if auth != "Bearer good-token":
            return self.send(401, {"error": "Invalid or expired API token"})
        method, params = request.get("method"), request.get("params") or {}
        if method == "tools/list":
            props = {"html_content": {"type": "string"}}
            if MODE != "old":
                props["expected_sha256"] = {"type": "string"}
            return self.result({"tools": [{"name": "create-page", "inputSchema": {"properties": props}}]})
        args = params.get("arguments") or {}
        name = params.get("name")
        if name == "list-pages":
            listed = [{"id": i, "route_path": p["route_path"], "url": f"https://prod.invalid/testuser/{p['route_path']}"}
                      for i, p in pages.items()]
            return self.result({"structuredContent": {"pages": listed, "last_page": 1}})
        if name in ("create-page", "update-page"):
            received = args["html_content"]
            if args.get("expected_sha256") and args["expected_sha256"] != sha(received):
                return self.result({"isError": True, "content": [{"type": "text", "text": "does not match expected_sha256. Nothing was written."}]})
            stored = received[:-10] if MODE == "mangle" else received
            page_id = args.get("page_id") or len(pages) + 1
            route = args.get("route_path") or pages[page_id]["route_path"]
            pages[page_id] = {"route_path": route, "html": stored}
            receipt = {"id": page_id, "url": f"https://prod.invalid/testuser/{route}"}
            if MODE != "old":
                receipt.update({"received_sha256": sha(stored), "stored_sha256": sha(stored), "stored_bytes": len(stored.encode()),
                                "verified": True, "tail_excerpt": stored[-120:], "warnings": []})
            return self.result({"structuredContent": receipt})
        return self.result({"isError": True, "content": [{"type": "text", "text": f"unknown tool {name}"}]})

    def result(self, value):
        self.send(200, {"jsonrpc": "2.0", "id": 1, "result": value})


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w") as handle:
    handle.write(str(server.server_address[1]))
server.serve_forever()
