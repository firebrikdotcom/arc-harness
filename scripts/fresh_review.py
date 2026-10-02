#!/usr/bin/env python3
"""Run an independent reviewer as a fresh process that sees only a review packet.

`scripts/review.sh` builds the packet (acceptance criteria and the target diff).
This runner starts the configured reviewer argv directly, never through a shell,
in an empty scratch directory outside the project, with the packet on stdin and
an allowlisted environment, then reads the reviewer's verdict line. It stores
the reviewer's output and a KEY=VALUE result in the output directory; it never
edits the project and cannot mark a phase done.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_USAGE = 2

COMMAND_FILE_LIMIT = 4096
DEFAULT_TIMEOUT_S = 1800
DEFAULT_MAX_BYTES = 2_000_000

# Only what a CLI needs to find itself, its user configuration, and a locale.
# Agent-session variables (CLAUDE_CODE_*, CODEX_*), harness state (HARNESS_*),
# and anything else stay behind unless the operator names them.
ENV_ALLOWLIST = (
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
)
ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
VERDICT_LINE = re.compile(r"^[\s>*_`]*VERDICT:\s*(PASS|FAIL)[\s*_`.]*$", re.IGNORECASE)


class UsageError(Exception):
    pass


def program_label(command: list[str]) -> str:
    """The program's base name, safe to store as one KEY=VALUE line."""
    return re.sub(r"[^A-Za-z0-9._+-]", "_", Path(command[0]).name) or "_"


def read_command(path: Path) -> tuple[list[str], bytes]:
    try:
        raw = path.read_bytes()
        if len(raw) > COMMAND_FILE_LIMIT:
            raise UsageError(f"reviewer command file exceeds {COMMAND_FILE_LIMIT} bytes: {path}")
        value = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise UsageError(f"cannot read reviewer command file {path}: {error}") from error
    # Later arguments may be empty strings, which some CLIs use to clear a list
    # (for example `claude --tools ""`); the program itself may not.
    if (
        not isinstance(value, list)
        or not value
        or any(not isinstance(item, str) for item in value)
        or not value[0]
    ):
        raise UsageError(f"reviewer command file must contain a JSON argv array of strings with a program first: {path}")
    if any("\0" in item for item in value):
        raise UsageError("reviewer command arguments must not contain NUL")
    try:
        for item in value:
            os.fsencode(item)
    except UnicodeEncodeError as error:
        raise UsageError("reviewer command arguments must be encodable for execution") from error
    return value, raw


def positive_int(name: str, raw: str | None, default: int) -> int:
    if raw is None or raw == "":
        return default
    if not raw.isdigit() or int(raw) <= 0:
        raise UsageError(f"{name} must be a positive integer, got {raw!r}")
    return int(raw)


def reviewer_environment(extra: str) -> dict[str, str]:
    env = {name: os.environ[name] for name in ENV_ALLOWLIST if name in os.environ}
    for name in extra.split():
        if not ENV_NAME.match(name):
            raise UsageError(f"HARNESS_REVIEWER_ENV holds an invalid variable name: {name!r}")
        if name.startswith("HARNESS_"):
            raise UsageError(f"HARNESS_REVIEWER_ENV may not pass harness state to the reviewer: {name}")
        if name == "CLAUDECODE" or name.startswith(("CODEX_", "CLAUDE_CODE_")):
            raise UsageError(f"HARNESS_REVIEWER_ENV may not pass agent-session state to the reviewer: {name}")
        if name in os.environ:
            env[name] = os.environ[name]
    # A reviewer that is itself an agent CLI must not register or bootstrap its
    # scratch directory through the SessionStart hook.
    env["HARNESS_AUTO_INIT"] = "0"
    return env


def parse_verdict(output: str) -> str:
    verdict = "none"
    for line in output.splitlines():
        match = VERDICT_LINE.match(line)
        if match:
            verdict = match.group(1).lower()
    return verdict


def write_result(out_dir: Path, fields: dict[str, str]) -> None:
    tmp = out_dir / "result.state.tmp"
    private_write(tmp, "".join(f"{key}={value}\n" for key, value in fields.items()).encode("utf-8"))
    tmp.replace(out_dir / "result.state")


def scratch_parent(excluded_roots: list[Path]) -> Path:
    roots = [root.resolve() for root in excluded_roots]
    # Never use TMPDIR: it may be inside the target or harness.
    for candidate in (Path("/tmp"), Path("/var/tmp")):
        parent = candidate.resolve()
        if parent.is_dir() and not any(parent == root or root in parent.parents for root in roots):
            return parent
    raise UsageError("no scratch directory outside the target and harness is available")


def private_write(path: Path, data: bytes) -> None:
    with private_open(path) as stream:
        stream.write(data)


def private_open(path: Path):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(descriptor, 0o600)
    return os.fdopen(descriptor, "wb")


def run_review(command: list[str], packet: bytes, out_dir: Path, timeout_s: int, max_bytes: int, extra_env: str, excluded_roots: list[Path]) -> int:
    out_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    out_dir.chmod(0o700)
    fields = {
        "PACKET_SHA256": hashlib.sha256(packet).hexdigest(),
        "PACKET_BYTES": str(len(packet)),
        "REVIEWER_PROGRAM": program_label(command),
        "REVIEWER_EXIT": "none",
        "VERDICT": "none",
    }
    if len(packet) > max_bytes:
        # A truncated diff cannot be reviewed; refuse rather than send part of it.
        fields["VERDICT"] = "too_large"
        write_result(out_dir, fields)
        print(f"FAIL: review packet is {len(packet)} bytes, over HARNESS_REVIEWER_MAX_BYTES={max_bytes}; the reviewer was not started.", file=sys.stderr)
        return EXIT_FAIL

    workdir = Path(tempfile.mkdtemp(prefix="harness-fresh-review.", dir=scratch_parent(excluded_roots)))
    try:
        packet_path = workdir / "REVIEW.md"
        private_write(packet_path, packet)
        env = reviewer_environment(extra_env)
        stdout_path = out_dir / "reviewer.out"
        stderr_path = out_dir / "reviewer.err"
        with private_open(stdout_path) as stdout, private_open(stderr_path) as stderr:
            try:
                process = subprocess.Popen(
                    command,
                    cwd=workdir,
                    env=env,
                    stdin=subprocess.PIPE,
                    stdout=stdout,
                    stderr=stderr,
                    start_new_session=True,
                )
            except OSError as error:
                fields["VERDICT"] = "not_started"
                write_result(out_dir, fields)
                print(f"FAIL: could not start the reviewer {command[0]!r}: {error}", file=sys.stderr)
                return EXIT_FAIL
            try:
                process.communicate(input=packet, timeout=timeout_s)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except OSError:
                    process.kill()
                process.wait()
                fields["REVIEWER_EXIT"] = str(process.returncode)
                fields["VERDICT"] = "timeout"
                write_result(out_dir, fields)
                print(f"FAIL: the reviewer did not finish within HARNESS_REVIEWER_TIMEOUT={timeout_s}s and was stopped.", file=sys.stderr)
                return EXIT_FAIL
        fields["REVIEWER_EXIT"] = str(process.returncode)
        verdict = parse_verdict(stdout_path.read_text(encoding="utf-8", errors="replace"))
        fields["VERDICT"] = verdict
        write_result(out_dir, fields)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    if process.returncode != 0:
        print(f"FAIL: the reviewer exited {process.returncode}.", file=sys.stderr)
        return EXIT_FAIL
    if verdict != "pass":
        return EXIT_FAIL
    return EXIT_PASS


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="action", required=True)
    check = sub.add_parser("check", help="validate a reviewer command file")
    check.add_argument("--command", required=True, type=Path)
    check.add_argument("--snapshot", type=Path)
    check.add_argument("--exclude-root", action="append", type=Path, default=[])
    run = sub.add_parser("run", help="run the reviewer on a packet")
    run.add_argument("--command", required=True, type=Path)
    run.add_argument("--packet", required=True, type=Path)
    run.add_argument("--out", required=True, type=Path)
    run.add_argument("--exclude-root", action="append", type=Path, default=[])
    run.add_argument("--expected-command-sha256")
    args = parser.parse_args(argv)

    try:
        command, command_bytes = read_command(args.command)
        command_sha256 = hashlib.sha256(command_bytes).hexdigest()
        scratch_parent(args.exclude_root)
        timeout_s = positive_int("HARNESS_REVIEWER_TIMEOUT", os.environ.get("HARNESS_REVIEWER_TIMEOUT"), DEFAULT_TIMEOUT_S)
        max_bytes = positive_int("HARNESS_REVIEWER_MAX_BYTES", os.environ.get("HARNESS_REVIEWER_MAX_BYTES"), DEFAULT_MAX_BYTES)
        extra_env = os.environ.get("HARNESS_REVIEWER_ENV", "")
        # Validate the names now so a bad list fails before verification runs.
        reviewer_environment(extra_env)
        if args.action == "check":
            if args.snapshot is not None:
                private_write(args.snapshot, command_bytes)
            # The review record keeps which configuration produced the verdict.
            print(f"REVIEWER_PROGRAM={program_label(command)}")
            print(f"REVIEWER_COMMAND_SHA256={command_sha256}")
            return EXIT_PASS
        if args.expected_command_sha256 and command_sha256 != args.expected_command_sha256:
            raise UsageError("reviewer command snapshot changed after validation")
        try:
            packet = args.packet.read_bytes()
        except OSError as error:
            raise UsageError(f"cannot read review packet {args.packet}: {error}") from error
        return run_review(command, packet, args.out, timeout_s, max_bytes, extra_env, args.exclude_root)
    except UsageError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return EXIT_USAGE


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
