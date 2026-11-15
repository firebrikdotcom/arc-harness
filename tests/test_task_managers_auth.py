#!/usr/bin/env python3
"""Offline tests for scripts/task_managers_auth.py against an in-test fake OAuth server."""
from __future__ import annotations

import argparse
import http.server
import io
import json
import os
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import task_managers_auth as auth  # noqa: E402

CLIENT_ID = "fixture-client-id"
CLIENT_SECRET = "fixture-client-secret-9f3a"
CODE = "fixture-auth-code-71c2"
OLD_ACCESS = "fixture-access-old-aa11"
OLD_REFRESH = "fixture-refresh-old-bb22"
SECRETS = (CLIENT_SECRET, CODE, "fixture-access-", "fixture-refresh-")


class FakeOAuth:
    """POST /authorization/token like Launchpad: rotates tokens, records what it was sent."""

    def __init__(self):
        self.mode = "ok"  # ok | fail | no_refresh_token | garbage
        self.delay = 0.0
        self.valid_refresh = OLD_REFRESH
        self.redirect_uri = None
        self.posts = []  # (type, query string) per token request
        self.count = 0
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                split = urllib.parse.urlsplit(self.path)
                length = int(self.headers.get("Content-Length") or 0)
                form = {k: v[0] for k, v in urllib.parse.parse_qs(self.rfile.read(length).decode()).items()}
                fake.posts.append((form.get("type"), split.query))
                time.sleep(fake.delay)
                if split.path != "/authorization/token":
                    return self._send(404, {"error": "not found"})
                if fake.mode == "fail":
                    # An upstream that echoes what it was sent must not make the caller leak it.
                    return self._send(500, {"error": "boom", "echo": form})
                if fake.mode == "garbage":
                    return self._send(200, {"hello": "world"})
                if form.get("client_id") != CLIENT_ID or form.get("client_secret") != CLIENT_SECRET \
                        or form.get("redirect_uri") != fake.redirect_uri:
                    return self._send(401, {"error": "bad client", "echo": form})
                if form.get("type") == "web_server":
                    if form.get("code") != CODE:
                        return self._send(400, {"error": "bad code"})
                elif form.get("type") == "refresh":
                    if form.get("refresh_token") != fake.valid_refresh:
                        return self._send(401, {"error": "stale refresh token"})
                else:
                    return self._send(400, {"error": "bad type"})
                fake.count += 1
                token = {"access_token": f"fixture-access-new-{fake.count}", "expires_in": 1209600}
                if fake.mode != "no_refresh_token":
                    fake.valid_refresh = token["refresh_token"] = f"fixture-refresh-new-{fake.count}"
                self._send(200, token)

            def _send(self, status, value):
                body = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class StubCtx:
    """The ctx members of the shared spec that the auth module uses."""

    def __init__(self, db_root, cfg):
        self.db_root = Path(db_root)
        self.provider = "basecamp"
        self.config = {"version": 1, "providers": {"basecamp": cfg}}
        self.emitted = []

    def provider_cfg(self, name=None):
        return dict(self.config["providers"][name or self.provider])

    def emit(self, obj):
        self.emitted.append(obj)
        print(json.dumps(obj))

    def fail(self, exit_code, code, message):
        self.emit({"error": {"code": code, "message": message}})
        raise SystemExit(exit_code)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class AuthCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.fake = FakeOAuth()
        self.addCleanup(self.fake.close)
        self.port = free_port()
        self.redirect = f"http://localhost:{self.port}/oauth/callback"
        self.fake.redirect_uri = self.redirect
        self.env_file = self.dir / "basecamp.env"
        self.write_env(self.redirect)
        self.token_file = self.dir / "basecamp.oauth"
        self.ctx = StubCtx(self.dir / "db", {"env_file": str(self.env_file), "token_file": str(self.token_file),
                                             "account_id": "1", "api_base": self.fake.base, "auth_base": self.fake.base})
        # Everything printed during a test is kept, and checked for secrets when it ends.
        self.out, self.err = io.StringIO(), io.StringIO()
        for name, stream in (("stdout", self.out), ("stderr", self.err)):
            patcher = mock.patch.object(sys, name, stream)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(self.assert_no_secret_printed)

    def write_env(self, redirect):
        self.env_file.write_text(
            f"# app credentials\nBASECAMP_CLIENT_ID={CLIENT_ID}\nBASECAMP_CLIENT_SECRET=\"{CLIENT_SECRET}\"\n"
            f"BASECAMP_REDIRECT_URI={redirect}\n", encoding="utf-8")

    def assert_no_secret_printed(self):
        printed = self.out.getvalue() + self.err.getvalue()
        for secret in SECRETS:
            self.assertNotIn(secret, printed)

    def save_old(self, **extra):
        token = {"access_token": OLD_ACCESS, "refresh_token": OLD_REFRESH, "expires_in": 1209600, **extra}
        self.token_file.write_text(json.dumps(token), encoding="utf-8")
        os.chmod(self.token_file, 0o600)
        return self.token_file.read_bytes()

    def stored(self):
        return json.loads(self.token_file.read_text(encoding="utf-8"))

    def mode(self):
        return stat.S_IMODE(self.token_file.stat().st_mode)

    def leftovers(self):
        return sorted(p.name for p in self.dir.iterdir() if p.name.endswith(".tmp"))

    # login helpers
    def start_login(self, timeout=10, no_browser=True):
        box = {}

        def run():
            try:
                box["token"] = auth.login(self.ctx, "basecamp", no_browser, timeout=timeout)
            except BaseException as error:  # handed to the test thread
                box["error"] = error

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        deadline = time.monotonic() + 5
        while "http" not in self.err.getvalue() and thread.is_alive() and time.monotonic() < deadline:
            time.sleep(0.01)
        return thread, box

    def printed_url(self):
        lines = [line for line in self.err.getvalue().splitlines() if line.startswith("http")]
        self.assertEqual(len(lines), 1)
        return lines[0]

    def visit(self, query, path="/oauth/callback", host="127.0.0.1"):
        url = f"http://{host}:{self.port}{path}?" + urllib.parse.urlencode(query)
        try:
            with urllib.request.urlopen(url, timeout=5) as response:
                return response.status
        except urllib.error.HTTPError as error:
            error.close()
            return error.code


class RefreshTests(AuthCase):
    def test_refresh_rotates_the_stored_token_atomically_at_0600(self):
        self.save_old()
        os.chmod(self.token_file, 0o644)  # a loose mode is tightened by the replace
        inode = self.token_file.stat().st_ino
        token = auth.refresh(self.ctx, "basecamp", now=1000)
        self.assertEqual(token, self.stored())
        self.assertEqual(token["access_token"], "fixture-access-new-1")
        self.assertEqual(token["refresh_token"], "fixture-refresh-new-1")
        self.assertEqual((token["obtained_at"], token["expires_at"]), (1000, 1000 + 1209600))
        self.assertEqual(self.mode(), 0o600)
        self.assertNotEqual(self.token_file.stat().st_ino, inode)  # replaced, not rewritten in place
        self.assertEqual(self.leftovers(), [])
        self.assertEqual(self.fake.posts, [("refresh", "")])  # the secret went in the body, not the URL

    def test_failed_refresh_leaves_the_old_token_intact(self):
        before = self.save_old()
        for mode in ("fail", "garbage"):
            self.fake.mode = mode
            with self.assertRaises(auth.AuthError) as caught:
                auth.refresh(self.ctx)
            for secret in SECRETS:
                self.assertNotIn(secret, str(caught.exception))
            self.assertEqual(self.token_file.read_bytes(), before)
            self.assertEqual(self.leftovers(), [])
        self.assertIn("HTTP 500", str(auth_error(lambda: self._refresh_in("fail"))))

    def _refresh_in(self, mode):
        self.fake.mode = mode
        auth.refresh(self.ctx)

    def test_unreachable_provider_fails_without_touching_the_token(self):
        before = self.save_old()
        self.ctx.config["providers"]["basecamp"]["auth_base"] = f"http://127.0.0.1:{free_port()}"
        with self.assertRaises(auth.AuthError):
            auth.refresh(self.ctx)
        self.assertEqual(self.token_file.read_bytes(), before)

    def test_missing_refresh_token_in_response_keeps_the_old_one(self):
        self.save_old()
        self.fake.mode = "no_refresh_token"
        token = auth.refresh(self.ctx)
        self.assertEqual(token["access_token"], "fixture-access-new-1")
        self.assertEqual(self.stored()["refresh_token"], OLD_REFRESH)

    def test_needs_refresh_near_expiry_only(self):
        token = {"access_token": "a", "expires_at": 10_000}
        self.assertFalse(auth.needs_refresh(token, 9_699))
        self.assertTrue(auth.needs_refresh(token, 9_700))
        self.assertTrue(auth.needs_refresh(token, 20_000))
        self.assertFalse(auth.needs_refresh(token, 9_990, skew=5))
        self.assertFalse(auth.needs_refresh({"obtained_at": 0, "expires_in": 10_000}, 9_699))
        self.assertTrue(auth.needs_refresh({"obtained_at": 0, "expires_in": 10_000}, 9_700))
        self.assertTrue(auth.needs_refresh({"access_token": "a", "expires_in": 10_000}, 0))  # expiry unknown

    def test_ensure_fresh_refreshes_near_expiry_and_not_otherwise(self):
        before = self.save_old(obtained_at=0, expires_at=10_000)
        token = auth.ensure_fresh(self.ctx, "basecamp", now=5_000)
        self.assertEqual(token["access_token"], OLD_ACCESS)
        self.assertEqual(self.fake.posts, [])
        self.assertEqual(self.token_file.read_bytes(), before)
        token = auth.ensure_fresh(self.ctx, "basecamp", now=9_800)
        self.assertEqual(token["access_token"], "fixture-access-new-1")
        self.assertEqual(len(self.fake.posts), 1)
        self.assertEqual(self.stored()["expires_at"], 9_800 + 1209600)
        auth.ensure_fresh(self.ctx, "basecamp", now=9_900)  # the new token is far from expiry
        self.assertEqual(len(self.fake.posts), 1)

    def test_concurrent_refreshes_send_one_request_and_agree(self):
        self.save_old()
        self.fake.delay = 0.3
        results, errors = [], []

        def run():
            try:
                results.append(auth.refresh(self.ctx))
            except BaseException as error:
                errors.append(error)

        threads = [threading.Thread(target=run) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(20)
        self.assertEqual(errors, [])
        self.assertEqual(len(self.fake.posts), 1)  # the others saw the new token under the lock
        self.assertEqual({r["access_token"] for r in results}, {"fixture-access-new-1"})
        self.assertEqual(self.stored()["refresh_token"], "fixture-refresh-new-1")
        self.assertEqual(self.mode(), 0o600)
        self.assertEqual(stat.S_IMODE(Path(str(self.token_file) + ".lock").stat().st_mode), 0o600)

    def test_refresh_without_a_token_or_refresh_token(self):
        with self.assertRaises(auth.AuthError) as caught:
            auth.refresh(self.ctx)
        self.assertEqual((caught.exception.code, caught.exception.exit_code), ("not_connected", 3))
        self.token_file.write_text(json.dumps({"access_token": OLD_ACCESS}), encoding="utf-8")
        with self.assertRaises(auth.AuthError) as caught:
            auth.refresh(self.ctx)
        self.assertEqual(caught.exception.code, "auth_failed")
        self.assertEqual(self.fake.posts, [])

    def test_main_refresh_emits_expires_at_and_failures_exit_5(self):
        self.save_old()
        args = argparse.Namespace(cmd="refresh", provider="basecamp")
        self.assertEqual(auth.main(args, self.ctx), 0)
        self.assertEqual(list(self.ctx.emitted[-1]), ["expires_at"])
        self.assertEqual(self.ctx.emitted[-1]["expires_at"], self.stored()["expires_at"])
        self.assertEqual(json.loads(self.out.getvalue()), self.ctx.emitted[-1])
        self.fake.mode = "fail"
        with self.assertRaises(SystemExit) as caught:
            auth.main(args, self.ctx)
        self.assertEqual(caught.exception.code, 5)
        self.assertEqual(self.ctx.emitted[-1]["error"]["code"], "auth_failed")

    def test_missing_credentials_name_the_key_not_a_value(self):
        self.save_old()
        self.env_file.write_text(f"BASECAMP_CLIENT_ID={CLIENT_ID}\nBASECAMP_CLIENT_SECRET={CLIENT_SECRET}\n", encoding="utf-8")
        with self.assertRaises(auth.AuthError) as caught:
            auth.refresh(self.ctx)
        self.assertIn("BASECAMP_REDIRECT_URI", str(caught.exception))
        self.assertNotIn(CLIENT_SECRET, str(caught.exception))


def auth_error(call):
    try:
        call()
    except auth.AuthError as error:
        return error
    raise AssertionError("no AuthError")


class LoginTests(AuthCase):
    def test_login_exchanges_the_code_and_rotates_the_stored_token(self):
        self.save_old()
        with mock.patch("webbrowser.open") as opened:
            thread, box = self.start_login()
            url = urllib.parse.urlsplit(self.printed_url())
            query = {k: v[0] for k, v in urllib.parse.parse_qs(url.query).items()}
            self.assertEqual(f"{url.scheme}://{url.netloc}{url.path}", self.fake.base + "/authorization/new")
            self.assertEqual({k: query[k] for k in ("type", "client_id", "redirect_uri")},
                             {"type": "web_server", "client_id": CLIENT_ID, "redirect_uri": self.redirect})
            self.assertGreaterEqual(len(query["state"]), 32)
            self.assertEqual(sorted(query), ["client_id", "redirect_uri", "state", "type"])
            self.assertEqual(self.visit({"x": "1"}, path="/elsewhere"), 404)  # a stray request does not end the wait
            self.assertEqual(self.visit({"code": CODE, "state": query["state"]}), 200)
            thread.join(10)
        opened.assert_not_called()  # --no-browser never opens one here
        self.assertNotIn("error", box)
        token = self.stored()
        self.assertEqual(token, box["token"])
        self.assertEqual(token["access_token"], "fixture-access-new-1")
        self.assertEqual(token["refresh_token"], "fixture-refresh-new-1")
        self.assertEqual(token["expires_at"], token["obtained_at"] + 1209600)
        self.assertLess(abs(token["obtained_at"] - time.time()), 60)
        self.assertEqual(self.mode(), 0o600)
        self.assertEqual(self.leftovers(), [])
        self.assertEqual(self.fake.posts, [("web_server", "")])
        self.assertEqual(self.out.getvalue(), "")  # the address goes to stderr only

    def test_login_opens_a_browser_by_default(self):
        with mock.patch("webbrowser.open") as opened:
            thread, box = self.start_login(no_browser=False)
            url = self.printed_url()
            state = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["state"][0]
            self.visit({"code": CODE, "state": state})
            thread.join(10)
        opened.assert_called_once_with(url)
        self.assertEqual(self.stored()["access_token"], "fixture-access-new-1")

    def test_wrong_state_is_refused(self):
        before = self.save_old()
        thread, box = self.start_login()
        self.printed_url()
        self.assertEqual(self.visit({"code": CODE, "state": "not-the-state"}), 400)
        thread.join(10)
        self.assertIsInstance(box.get("error"), auth.AuthError)
        self.assertIn("state", str(box["error"]))
        self.assertEqual(self.fake.posts, [])  # the code was never exchanged
        self.assertEqual(self.token_file.read_bytes(), before)

    def test_missing_state_and_provider_error_are_refused(self):
        thread, box = self.start_login()
        self.printed_url()
        self.assertEqual(self.visit({"code": CODE}), 400)
        thread.join(10)
        self.assertIsInstance(box.get("error"), auth.AuthError)
        self.assertFalse(self.token_file.exists())

        self.err.truncate(0)
        self.err.seek(0)
        thread, box = self.start_login()
        state = urllib.parse.parse_qs(urllib.parse.urlsplit(self.printed_url()).query)["state"][0]
        self.assertEqual(self.visit({"error": "access_denied", "state": state}), 400)
        thread.join(10)
        self.assertIsInstance(box.get("error"), auth.AuthError)
        self.assertEqual(self.fake.posts, [])
        self.assertFalse(self.token_file.exists())

    def test_failed_exchange_leaves_the_old_token_intact(self):
        before = self.save_old()
        self.fake.mode = "fail"
        thread, box = self.start_login()
        state = urllib.parse.parse_qs(urllib.parse.urlsplit(self.printed_url()).query)["state"][0]
        self.assertEqual(self.visit({"code": CODE, "state": state}), 200)
        thread.join(10)
        self.assertIsInstance(box.get("error"), auth.AuthError)
        for secret in SECRETS:
            self.assertNotIn(secret, str(box["error"]))
        self.assertEqual(self.token_file.read_bytes(), before)
        self.assertEqual(self.leftovers(), [])

    def test_login_times_out(self):
        started = time.monotonic()
        with self.assertRaises(auth.AuthError) as caught:
            auth.login(self.ctx, "basecamp", True, timeout=0.3)
        self.assertIn("timed out", str(caught.exception))
        self.assertLess(time.monotonic() - started, 5)
        self.assertFalse(self.token_file.exists())
        with mock.patch.dict(os.environ, {auth.LOGIN_TIMEOUT_ENV: "0.2"}):
            self.assertEqual(auth._login_timeout(None), 0.2)
        self.assertEqual(auth._login_timeout(None), 300)

    def test_callback_server_binds_loopback_only(self):
        server = auth.callback_server(self.redirect, "s", {})  # redirect host is "localhost"
        try:
            self.assertEqual(server.server_address, ("127.0.0.1", self.port))
            self.assertEqual(server.socket.getsockname()[0], "127.0.0.1")
        finally:
            server.server_close()
        for bad in (f"http://0.0.0.0:{self.port}/cb", f"http://example.com:{self.port}/cb", f"http://[::]:{self.port}/cb",
                    f"https://127.0.0.1:{self.port}/cb", "http://127.0.0.1/cb", "http://127.0.0.1:notaport/cb"):
            with self.assertRaises(auth.AuthError, msg=bad):
                auth.callback_server(bad, "s", {})

    def test_login_refuses_a_non_loopback_redirect_before_printing_anything(self):
        self.write_env(f"http://0.0.0.0:{self.port}/oauth/callback")
        with self.assertRaises(auth.AuthError):
            auth.login(self.ctx, "basecamp", True, timeout=0.2)
        self.assertEqual(self.err.getvalue(), "")

    def test_main_login_emits_connected(self):
        box = {}

        def run():
            try:
                box["exit"] = auth.main(argparse.Namespace(cmd="login", provider="basecamp", no_browser=True), self.ctx)
            except BaseException as error:
                box["error"] = error

        with mock.patch.dict(os.environ, {auth.LOGIN_TIMEOUT_ENV: "10"}):
            thread = threading.Thread(target=run, daemon=True)
            thread.start()
            deadline = time.monotonic() + 5
            while "http" not in self.err.getvalue() and time.monotonic() < deadline:
                time.sleep(0.01)
            state = urllib.parse.parse_qs(urllib.parse.urlsplit(self.printed_url()).query)["state"][0]
            self.visit({"code": CODE, "state": state})
            thread.join(10)
        self.assertEqual(box, {"exit": 0})
        self.assertEqual(self.ctx.emitted, [{"connected": True, "expires_at": self.stored()["expires_at"]}])
        self.assertEqual(json.loads(self.out.getvalue()), self.ctx.emitted[0])

        with mock.patch.dict(os.environ, {auth.LOGIN_TIMEOUT_ENV: "0.2"}), self.assertRaises(SystemExit) as caught:
            auth.main(argparse.Namespace(cmd="login", provider="basecamp", no_browser=True), self.ctx)
        self.assertEqual(caught.exception.code, 5)
        self.assertEqual(self.ctx.emitted[-1]["error"]["code"], "auth_failed")


@unittest.skipUnless((ROOT / "scripts" / "task_managers.py").exists(), "scripts/task_managers.py is not in this checkout")
class CommandLineTests(AuthCase):
    """The real entry point reaches this module; its output carries no secret."""

    def run_cli(self, *argv):
        home = self.dir / "config"
        home.mkdir(exist_ok=True)
        (home / "task-managers.json").write_text(json.dumps({
            "version": 1, "owner_login": None, "providers": {"basecamp": self.ctx.config["providers"]["basecamp"]},
            "repos": {}, "mappings": {},
            "run": {"plan_profile": "tm_plan", "build_profile": "tm_build", "review_profile": "tm_review", "base": "origin/HEAD"},
        }), encoding="utf-8")
        env = {**os.environ, "HARNESS_CONFIG_HOME": str(home)}
        done = subprocess.run([sys.executable, str(ROOT / "scripts" / "task_managers.py"), "--db-root", str(self.dir / "db"), *argv],
                              capture_output=True, text=True, timeout=60, env=env)
        for secret in SECRETS:
            self.assertNotIn(secret, done.stdout + done.stderr)
        return done

    def test_refresh_command(self):
        self.save_old()
        done = self.run_cli("refresh")
        self.assertEqual(done.returncode, 0, done.stdout)
        self.assertEqual(json.loads(done.stdout), {"expires_at": self.stored()["expires_at"]})
        self.assertEqual(self.stored()["access_token"], "fixture-access-new-1")
        self.assertEqual(self.mode(), 0o600)
        self.fake.mode = "fail"
        done = self.run_cli("refresh")
        self.assertEqual(done.returncode, 5, done.stdout)
        self.assertEqual(json.loads(done.stdout)["error"]["code"], "auth_failed")
        self.assertEqual(self.stored()["access_token"], "fixture-access-new-1")


@unittest.skipUnless((ROOT / "scripts" / "task_managers.py").exists() and (ROOT / "tests" / "fake_basecamp.py").exists(),
                     "the reader or its Basecamp fake is not in this checkout")
class FakeBasecampTests(unittest.TestCase):
    """The real CLI against tests/fake_basecamp.py in `expired` mode: the first access
    token gets 401, tokens issued by its token endpoint are accepted."""

    def setUp(self):
        sys.path.insert(0, str(ROOT / "tests"))
        self.addCleanup(sys.path.remove, str(ROOT / "tests"))
        import fake_basecamp
        self.fb = fake_basecamp
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        port_file = self.dir / "port"
        self.log = self.dir / "requests.log"
        server = subprocess.Popen([sys.executable, str(ROOT / "tests" / "fake_basecamp.py"), str(port_file), "expired", str(self.log)])
        self.addCleanup(server.wait)
        self.addCleanup(server.kill)
        deadline = time.monotonic() + 10
        while not (port_file.exists() and port_file.read_text()):
            self.assertLess(time.monotonic(), deadline, "fake_basecamp did not start")
            time.sleep(0.02)
        base = f"http://127.0.0.1:{port_file.read_text()}"
        env_file = self.dir / "basecamp.env"
        env_file.write_text(f"BASECAMP_CLIENT_ID={self.fb.CLIENT_ID}\nBASECAMP_CLIENT_SECRET={self.fb.CLIENT_SECRET}\n"
                            f"BASECAMP_REDIRECT_URI={self.fb.REDIRECT_URI}\n", encoding="utf-8")
        self.token_file = self.dir / "basecamp.oauth"
        self.token_file.write_text(json.dumps({"access_token": self.fb.ACCESS_TOKEN, "refresh_token": self.fb.REFRESH_TOKEN,
                                               "expires_in": self.fb.EXPIRES_IN}), encoding="utf-8")
        os.chmod(self.token_file, 0o600)
        home = self.dir / "config"
        home.mkdir()
        (home / "task-managers.json").write_text(json.dumps({
            "version": 1, "owner_login": None, "repos": {}, "mappings": {},
            "providers": {"basecamp": {"env_file": str(env_file), "token_file": str(self.token_file),
                                       "account_id": self.fb.ACCOUNT_ID, "api_base": base, "auth_base": base}},
            "run": {"plan_profile": "tm_plan", "build_profile": "tm_build", "review_profile": "tm_review", "base": "origin/HEAD"},
        }), encoding="utf-8")
        self.env = {**os.environ, "HARNESS_CONFIG_HOME": str(home)}

    def run_cli(self, *argv):
        done = subprocess.run([sys.executable, str(ROOT / "scripts" / "task_managers.py"), "--db-root", str(self.dir / "db"), *argv],
                              capture_output=True, text=True, timeout=60, env=self.env)
        stored = json.loads(self.token_file.read_text(encoding="utf-8"))
        for secret in (self.fb.CLIENT_SECRET, self.fb.ACCESS_TOKEN, self.fb.REFRESH_TOKEN, self.fb.AUTH_CODE,
                       stored["access_token"], stored["refresh_token"]):
            self.assertNotIn(secret, done.stdout + done.stderr)
        return done, stored

    def test_refresh_command_rotates_the_token(self):
        done, stored = self.run_cli("refresh")
        self.assertEqual(done.returncode, 0, done.stdout)
        self.assertEqual(json.loads(done.stdout), {"expires_at": stored["expires_at"]})
        self.assertNotEqual(stored["access_token"], self.fb.ACCESS_TOKEN)
        self.assertNotEqual(stored["refresh_token"], self.fb.REFRESH_TOKEN)
        self.assertEqual(stored["expires_at"], stored["obtained_at"] + self.fb.EXPIRES_IN)
        self.assertEqual(stat.S_IMODE(self.token_file.stat().st_mode), 0o600)
        self.assertNotIn(self.fb.CLIENT_SECRET, self.log.read_text(encoding="utf-8"))  # nothing secret in a logged URL
        done, _ = self.run_cli("projects")  # the issued token is accepted
        self.assertEqual(done.returncode, 0, done.stdout)
        self.assertIn("projects", json.loads(done.stdout))

    def test_a_401_on_a_read_renews_the_token_and_retries(self):
        done, stored = self.run_cli("projects")
        self.assertEqual(done.returncode, 0, done.stdout)
        self.assertIn("projects", json.loads(done.stdout))
        self.assertNotEqual(stored["access_token"], self.fb.ACCESS_TOKEN)
        self.assertEqual(stat.S_IMODE(self.token_file.stat().st_mode), 0o600)

    def test_a_failed_renewal_surfaces_as_exit_5_with_the_token_untouched(self):
        before = self.token_file.read_bytes()
        self.token_file.write_text(json.dumps({**json.loads(before), "refresh_token": "fake-refresh-token-stale"}), encoding="utf-8")
        before = self.token_file.read_bytes()
        for command in ("refresh", "projects"):
            done, _ = self.run_cli(command)
            self.assertEqual(done.returncode, 5, done.stdout)
            self.assertIn("code", json.loads(done.stdout)["error"])
            self.assertNotIn("fake-refresh-token-stale", done.stdout + done.stderr)
            self.assertEqual(self.token_file.read_bytes(), before)


class ErrorFamilyTests(unittest.TestCase):
    def test_auth_error_is_an_upstream_error_when_the_reader_is_importable(self):
        error = auth.AuthError("x")
        self.assertEqual((str(error), error.code, error.exit_code), ("x", "auth_failed", 5))
        try:
            import task_managers
        except ImportError:
            self.skipTest("scripts/task_managers.py is not in this checkout")
        self.assertIsInstance(error, task_managers.Upstream)


if __name__ == "__main__":
    unittest.main()
