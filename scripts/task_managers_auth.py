#!/usr/bin/env python3
"""Task-manager sign-in: the browser login, and renewing the saved token.

`login` prints the provider's authorization URL, waits on a loopback-only one-shot
HTTP server for the redirect, checks `state`, and exchanges the code. `refresh`
trades the saved refresh token for a new access token. Both replace the token
file atomically (mode 0600) while holding a lock on a sidecar file, so two
processes renewing at once make one request and neither overwrites the other.

No message, log line or output of this module carries the client secret, a
token, or the authorization code. Standard library only.
"""
from __future__ import annotations

import contextlib
import fcntl
import hmac
import http.server
import json
import os
import secrets
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

USER_AGENT = "Harness Task Managers"
HTTP_TIMEOUT_SECONDS = 30
MAX_RESPONSE_BYTES = 1024 * 1024
LOGIN_TIMEOUT_SECONDS = 300
LOGIN_TIMEOUT_ENV = "HARNESS_TASK_MANAGERS_LOGIN_TIMEOUT"
CALLBACK_SOCKET_SECONDS = 10
REFRESH_SKEW_SECONDS = 300
LOOPBACK = "127.0.0.1"
LOOPBACK_HOSTS = ("127.0.0.1", "localhost")
ENV_KEYS = {"basecamp": ("BASECAMP_CLIENT_ID", "BASECAMP_CLIENT_SECRET", "BASECAMP_REDIRECT_URI")}


try:  # the entry point's error family when it is there, so its callers can catch ours
    from task_managers import Upstream as _ErrorBase
except ImportError:
    _ErrorBase = Exception


class AuthError(_ErrorBase):
    """A sign-in failure. The message is safe to print: it never holds a secret."""

    def __init__(self, message: str, code: str = "auth_failed", exit_code: int = 5):
        Exception.__init__(self, message)
        self.code = code
        self.exit_code = exit_code


# --- credentials and the token file ------------------------------------------

def load_credentials(env_file: str | Path, provider: str = "basecamp") -> dict[str, str]:
    """client_id, client_secret and redirect_uri from the KEY=VALUE env file."""
    keys = ENV_KEYS.get(provider)
    if keys is None:
        raise AuthError(f"sign-in is not built for provider {provider}")
    try:
        text = Path(env_file).read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise AuthError(f"cannot read the provider's credentials file: {type(error).__name__}") from None
    found = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        found[key.strip()] = value.strip().strip('"').strip("'")
    missing = [key for key in keys if not found.get(key)]
    if missing:
        raise AuthError("the provider's credentials file lacks " + ", ".join(missing))
    return dict(zip(("client_id", "client_secret", "redirect_uri"), (found[key] for key in keys)))


def read_token_file(file: str | Path) -> dict[str, Any]:
    try:
        token = json.loads(Path(file).read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise AuthError("not connected: no saved token; run the login command", "not_connected", 3) from None
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise AuthError(f"cannot read the token file: {type(error).__name__}") from None
    if not isinstance(token, dict):
        raise AuthError("the token file does not hold a token; run the login command")
    return token


def write_token_file(file: str | Path, token: dict[str, Any]) -> None:
    """Atomic 0600 replace: a reader sees the old token or the new one, never a part."""
    file = Path(file)
    file.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix="." + file.name + ".", suffix=".tmp", dir=file.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(token, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, file)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


@contextlib.contextmanager
def token_lock(file: str | Path):
    """Exclusive lock on `<token file>.lock`, held across re-read, request and replace."""
    file = Path(file)
    file.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(str(file) + ".lock", os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)  # closing releases the lock


# --- expiry ------------------------------------------------------------------

def _number(value: Any) -> float | None:
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else None


def expires_at(token: dict[str, Any]) -> float | None:
    """Epoch seconds the access token ends, or None when the file does not say."""
    direct = _number(token.get("expires_at"))
    if direct is not None:
        return direct
    obtained, lasts = _number(token.get("obtained_at")), _number(token.get("expires_in"))
    return obtained + lasts if obtained is not None and lasts is not None else None


def needs_refresh(token: dict[str, Any], now: float, skew: float = REFRESH_SKEW_SECONDS) -> bool:
    """True within `skew` seconds of expiry, after it, or when the expiry is unknown."""
    end = expires_at(token)
    return end is None or now >= end - skew


def stamped(response: dict[str, Any], previous: dict[str, Any] | None, now: float) -> dict[str, Any]:
    """The token to store: the response, the old refresh token when it sent none, and times."""
    token = dict(response)
    if not token.get("refresh_token") and previous and previous.get("refresh_token"):
        token["refresh_token"] = previous["refresh_token"]
    token["obtained_at"] = int(now)
    lasts = _number(token.get("expires_in"))
    token["expires_at"] = int(now + lasts) if lasts is not None and lasts > 0 else None
    return token


# --- the token endpoint ------------------------------------------------------

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    # A redirect would resend the client secret to another address; treat it as a failure.
    def redirect_request(self, *args, **kwargs):
        return None


def token_request(auth_base: str, fields: dict[str, str], what: str) -> dict[str, Any]:
    """POST the form to the token endpoint. Secrets travel in the body, never the URL."""
    url = auth_base.rstrip("/") + "/authorization/token"
    if urllib.parse.urlsplit(url).scheme not in ("http", "https"):
        raise AuthError("the provider's auth_base is not an http(s) address")
    request = urllib.request.Request(
        url, method="POST", data=urllib.parse.urlencode(fields).encode("ascii"),
        headers={"User-Agent": USER_AGENT, "Accept": "application/json",
                 "Content-Type": "application/x-www-form-urlencoded"})
    host = urllib.parse.urlsplit(url).netloc
    try:
        with urllib.request.build_opener(_NoRedirect).open(request, timeout=HTTP_TIMEOUT_SECONDS) as response:
            body = response.read(MAX_RESPONSE_BYTES + 1)
    except urllib.error.HTTPError as error:
        error.close()
        raise AuthError(f"the provider refused the {what} (HTTP {error.code}); run the login command") from None
    except (urllib.error.URLError, OSError, ValueError) as error:
        reason = getattr(error, "reason", error)
        raise AuthError(f"cannot reach {host} for the {what}: {type(reason).__name__}") from None
    try:
        if len(body) > MAX_RESPONSE_BYTES:
            raise ValueError
        token = json.loads(body.decode("utf-8"))
    except (UnicodeError, ValueError):
        raise AuthError(f"the provider's answer to the {what} is not a token") from None
    if not isinstance(token, dict) or not isinstance(token.get("access_token"), str) or not token["access_token"].strip():
        raise AuthError(f"the provider's answer to the {what} holds no access token")
    return token


# --- refresh -----------------------------------------------------------------

def refresh(ctx, provider: str = "basecamp", *, now: float | None = None) -> dict[str, Any]:
    """Renew the saved token and return the stored dict. Raises AuthError.

    The token is read before and after taking the lock: when another process
    renewed it in between, that token is returned and no request is sent.
    """
    cfg = ctx.provider_cfg(provider)
    file = Path(cfg["token_file"])
    before = read_token_file(file)
    with token_lock(file):
        current = read_token_file(file)
        if any(current.get(key) != before.get(key) for key in ("access_token", "refresh_token")):
            return current
        if not isinstance(current.get("refresh_token"), str) or not current["refresh_token"]:
            raise AuthError("the saved token cannot be renewed (no refresh token); run the login command")
        creds = load_credentials(cfg["env_file"], provider)
        response = token_request(cfg["auth_base"], {
            "type": "refresh", "refresh_token": current["refresh_token"], "client_id": creds["client_id"],
            "client_secret": creds["client_secret"], "redirect_uri": creds["redirect_uri"]}, "token renewal")
        token = stamped(response, current, time.time() if now is None else now)
        write_token_file(file, token)
        return token


def ensure_fresh(ctx, provider: str = "basecamp", *, skew: float = REFRESH_SKEW_SECONDS, now: float | None = None) -> dict[str, Any]:
    """The saved token, renewed first when it is within `skew` seconds of expiry."""
    token = read_token_file(ctx.provider_cfg(provider)["token_file"])
    if needs_refresh(token, time.time() if now is None else now, skew):
        return refresh(ctx, provider, now=now)
    return token


# --- login -------------------------------------------------------------------

def authorization_url(auth_base: str, client_id: str, redirect_uri: str, state: str) -> str:
    query = {"type": "web_server", "client_id": client_id, "redirect_uri": redirect_uri, "state": state}
    return auth_base.rstrip("/") + "/authorization/new?" + urllib.parse.urlencode(query)


def callback_server(redirect_uri: str, state: str, result: dict[str, Any]) -> http.server.HTTPServer:
    """A server on 127.0.0.1 only, at the redirect URI's port; the first request to its
    path fills `result` with `code`, or with `error` when it is refused."""
    parsed = urllib.parse.urlsplit(redirect_uri)
    try:
        port = parsed.port
    except ValueError:
        port = None
    if parsed.scheme != "http" or parsed.hostname not in LOOPBACK_HOSTS or not port:
        raise AuthError("the redirect URI must be http://127.0.0.1:PORT/PATH or http://localhost:PORT/PATH, "
                        "so the login can be received on this machine only")
    want_path = parsed.path or "/"

    class Handler(http.server.BaseHTTPRequestHandler):
        timeout = CALLBACK_SOCKET_SECONDS  # a silent connection cannot hold the login open

        def do_GET(self):
            got = urllib.parse.urlsplit(self.path)
            if got.path != want_path:
                return self._answer(404, "Not found.")
            query = {key: values[0] for key, values in urllib.parse.parse_qs(got.query).items()}
            if not hmac.compare_digest(query.get("state", "").encode("utf-8"), state.encode("utf-8")):
                result["error"] = "the redirect carried the wrong state; the login was refused"
                return self._answer(400, "Login refused: this redirect does not belong to the login in progress.")
            if "error" in query or not query.get("code"):
                result["error"] = "the provider did not grant access"
                return self._answer(400, "Login not completed: access was not granted.")
            result["code"] = query["code"]
            self._answer(200, "Login received. You can close this tab.")

        def _answer(self, status: int, text: str) -> None:
            body = text.encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):  # the request line holds the code: never log it
            pass

    try:
        return http.server.HTTPServer((LOOPBACK, port), Handler)
    except OSError as error:
        raise AuthError(f"cannot listen on {LOOPBACK}:{port} for the redirect: {type(error).__name__}") from None


def _login_timeout(timeout: float | None) -> float:
    if timeout is not None:
        return timeout
    try:
        return float(os.environ[LOGIN_TIMEOUT_ENV])
    except (KeyError, ValueError):
        return LOGIN_TIMEOUT_SECONDS


def login(ctx, provider: str = "basecamp", no_browser: bool = False, *, timeout: float | None = None) -> dict[str, Any]:
    """Run the browser login and store the token; returns the stored dict. Raises AuthError."""
    cfg = ctx.provider_cfg(provider)
    creds = load_credentials(cfg["env_file"], provider)
    state = secrets.token_urlsafe(32)
    result: dict[str, Any] = {}
    server = callback_server(creds["redirect_uri"], state, result)
    try:
        url = authorization_url(cfg["auth_base"], creds["client_id"], creds["redirect_uri"], state)
        print("Open this address in a browser to connect " + provider + ":\n" + url, file=sys.stderr, flush=True)
        if not no_browser:
            try:
                import webbrowser
                webbrowser.open(url)
            except Exception:  # the printed address is the fallback
                pass
        deadline = time.monotonic() + _login_timeout(timeout)
        while not result:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AuthError("timed out waiting for the browser redirect")
            server.timeout = remaining
            server.handle_request()
    finally:
        server.server_close()
    if "error" in result:
        raise AuthError(result["error"])
    response = token_request(cfg["auth_base"], {
        "type": "web_server", "code": result["code"], "client_id": creds["client_id"],
        "client_secret": creds["client_secret"], "redirect_uri": creds["redirect_uri"]}, "code exchange")
    file = Path(cfg["token_file"])
    with token_lock(file):
        token = stamped(response, None, time.time())
        write_token_file(file, token)
    return token


# --- command adapter ---------------------------------------------------------

def main(args, ctx) -> int:
    provider = getattr(args, "provider", None) or getattr(ctx, "provider", None) or "basecamp"
    try:
        if args.cmd == "login":
            token = login(ctx, provider, bool(getattr(args, "no_browser", False)))
            ctx.emit({"connected": True, "expires_at": token.get("expires_at")})
        elif args.cmd == "refresh":
            ctx.emit({"expires_at": refresh(ctx, provider).get("expires_at")})
        else:
            ctx.fail(2, "usage", f"task_managers_auth has no command {args.cmd}")
    except AuthError as error:
        ctx.fail(error.exit_code, error.code, str(error))
    return 0
