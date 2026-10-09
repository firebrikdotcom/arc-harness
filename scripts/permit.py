#!/usr/bin/env python3
"""Denylist evaluation for shell commands and write paths.

Rules come from the project's .harness-denylist or schemas/denylist.default:

  command <regex>  matches the whole command text (destructive actions such as rm -rf /)
  path <regex>     matches a write target, relative to the project root when inside it
  inline <regex>   matches inline interpreter code (python -c, node -e, heredocs fed to
                   an interpreter, ...)

A shell command is parsed into segments (following cd/pushd, env -C, sh -c, eval,
heredocs and here-strings fed to a shell), and every file it would write is
checked against the path rules: redirections, tee, cp/mv/install/ln, rm/rmdir,
touch/mkdir, truncate, chmod/chown, dd of=, sed -i, perl -i, find -delete/-exec,
git rm/mv/checkout/restore, rsync. Deleting, moving, or changing the mode of a
directory is judged against everything beneath it, globs and braces against every
protected path they could name, and a target built from a variable by its literal
suffix. Commands whose writes cannot be known (xargs into a writer, tar, patch, ...)
and commands that cannot be parsed are refused when they name a protected path.
Reading or mentioning a protected file is allowed; writing it is not.

  permit.py check --command STRING [--project PATH] [--cwd PATH]
  permit.py check --path PATH [--project PATH] [--cwd PATH]
  permit.py targets --command STRING [--project PATH] [--cwd PATH]
  permit.py harness-call --command STRING [--project PATH] [--cwd PATH]
  permit.py rules [--project PATH]

Exit 0 allowed, 1 denied, 2 usage or environment error.
"""
from __future__ import annotations

import argparse
import fnmatch
import glob
import os
import re
import shlex
import shutil
import sys
from pathlib import Path

HARNESS_ROOT = Path(__file__).resolve().parent.parent
SEPARATORS = {";", "&&", "||", "|", "&", "|&", ";;", "(", ")", "{", "}"}
REDIRECT_OUT = {">", ">>", ">|", "&>", "&>>", "<>"}
REDIRECT_IN = {"<", "<&"}
SHELLS = {"sh", "bash", "zsh", "dash", "ksh"}
INTERPRETERS = {"python", "python2", "python3", "node", "perl", "ruby", "php", "deno", "bun"}
WRITERS = {"tee", "cp", "mv", "install", "ln", "rm", "rmdir", "unlink", "shred", "touch", "mkdir", "truncate",
           "chmod", "chown", "chgrp", "dd", "sed", "perl", "rsync", "git", "find"}
OPAQUE_WRITERS = {"tar", "unzip", "patch", "cpio", "bsdtar", "7z", "gunzip", "bunzip2", "xz", "zstd"}
VARIABLE = re.compile(r"\$\{[^}]*\}|\$\([^)]*\)|\$\w+|`[^`]*`")
GLOB_CHARS = re.compile(r"[*?\[]")
CHMOD_MODE = re.compile(r"^([-+=]?[0-7]{1,4}|[ugoa]*([-+=][rwxXstugo]*)+(,[ugoa]*([-+=][rwxXstugo]*)+)*)$")
# Paths the default rules protect; a rule set decides which of them actually are.
PROBES = (".git/HEAD", ".env", ".ssh/id_rsa", "scripts/hooks/require-phase.sh", "scripts/permit.sh",
          "scripts/permit.py", "scripts/guard-version", "scripts/knowledge-trust.sh", "schemas/denylist.default",
          ".harness-denylist", ".harness-db/runs/x/state", ".harness-db/records/verify.state",
          ".harness-db/trust/x", ".harness-db/targets/x/db/records/verify.state", ".claude/settings.json",
          ".claude/settings.local.json", ".claude/CLAUDE.md")


class Unparseable(Exception):
    pass


# Harness commands that are a person's (or the configured reviewer's) to run, and
# the settings that tune or disable the guard; both are judged on the parsed argv,
# so quoting or option order cannot hide them.
HUMAN_ONLY = {"abort", "failure", "launch"}
PASSABLE = {"plan", "build", "review", "status", "step", "continue", "contract", "brief", "workflow", "prune",
            "advise", "route", "help", "--help", "-h"}
GUARD_VARIABLES = {"HARNESS_HOOK_DISABLE", "HARNESS_RUN_IDLE_HOURS", "HARNESS_RETAIN_RUNS", "HARNESS_HOME",
                   "HARNESS_DB_ROOT", "HARNESS_REQUIRED_CHECKS", "HARNESS_REVIEWER_CMD", "HARNESS_REVIEW_BASE"}


ASSIGNMENT = re.compile(r"^[A-Za-z_]\w*\+?=")
# Builtins that set a variable named by an argument (read NAME, printf -v NAME, ...).
SETTERS = {"read", "printf", "mapfile", "readarray", "getopts", "declare", "typeset", "local", "export", "readonly"}


def assigned_name(word: str) -> str:
    return word.split("=", 1)[0].rstrip("+")


def guard_variable(name: str) -> bool:
    return name in GUARD_VARIABLES or name.startswith("HARNESS_BUDGET_")


def is_harness_cli(word: str, cwd: str) -> bool:
    if os.path.basename(word) == "harness":
        return True
    if "/" in word:
        return os.path.realpath(os.path.join(cwd, word)) == os.path.realpath(HARNESS_ROOT / "scripts" / "harness")
    return False


def harness_subcommand(argv: list[str]) -> tuple[str, str, bool]:
    """(subcommand, its first argument, whether a --session-id was given)."""
    rest, session = argv[1:], False
    while rest and rest[0].startswith("-") and rest[0] not in ("--help", "-h"):
        session = session or rest[0] == "--session-id"
        rest = rest[2:] if rest[0] == "--session-id" else rest[1:]
    return (rest[0] if rest else ""), (rest[1] if len(rest) > 1 else ""), session


def human_only(argv: list[str]) -> str | None:
    name = os.path.basename(argv[0]) if argv else ""
    if name == "harness":
        sub, second, _ = harness_subcommand(argv)
        if sub in HUMAN_ONLY or (sub == "review" and second == "submit"):
            return f"harness {sub}{' ' + second if sub == 'review' else ''}"
    if name == "knowledge-trust.sh" and "approve" in argv[1:]:
        return "knowledge-trust approve"
    return None


class Analysis:
    def __init__(self) -> None:
        self.targets: list[tuple[str, tuple[str, ...], str]] = []  # (target, cwd candidates, file|tree)
        self.inline: list[str] = []
        self.opaque = False
        self.words: list[str] = []
        self.refusal: str | None = None  # a human-only command or a guard setting


def load_rules(project: Path) -> tuple[Path, list[tuple[str, str, int]]]:
    path = project / ".harness-denylist"
    if not path.is_file():
        path = HARNESS_ROOT / "schemas" / "denylist.default"
    if not path.is_file():
        raise FileNotFoundError(f"no denylist found at {path}")
    rules = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        kind, _, pattern = line.partition(" ")
        try:
            re.compile(pattern)
        except re.error as error:
            raise ValueError(f"bad regex at {path}:{number}: {error}") from error
        rules.append((kind, pattern, number))
    return path, rules


def first_match(rules, rules_path, kind: str, subject: str) -> str | None:
    for rule_kind, pattern, number in rules:
        if rule_kind == kind and re.search(pattern, subject):
            return f"DENY: {kind} matches {rules_path}:{number}: {pattern}"
    return None


# --- parsing ------------------------------------------------------------------

def preprocess(command: str) -> tuple[str, list[str]]:
    """Join physical lines into one ;-separated command, quote-aware: drop comments
    and line continuations, and lift heredoc bodies out (returned in order)."""
    out: list[str] = []
    bodies: list[str] = []
    pending: list[tuple[str, bool]] = []
    quote = None
    word_start = True
    index, length = 0, len(command)
    while index < length:
        char = command[index]
        if quote:
            out.append(char)
            if quote == '"' and char == "\\" and index + 1 < length:
                out.append(command[index + 1])
                index += 2
                continue
            if char == quote:
                quote = None
            index += 1
            continue
        if char == "\\" and index + 1 < length:
            if command[index + 1] != "\n":
                out.append(command[index:index + 2])
            index += 2
            word_start = False
            continue
        if char in "'\"":
            quote = char
            out.append(char)
            word_start = False
            index += 1
            continue
        if char == "#" and word_start:
            newline = command.find("\n", index)
            index = length if newline < 0 else newline
            continue
        if command.startswith("<<", index) and not command.startswith("<<<", index):
            match = re.match(r"<<(-?)[ \t]*(['\"]?)([^\s'\";&|<>()]+)\2", command[index:])
            if match:
                pending.append((match.group(3), bool(match.group(1))))
                out.append(" << " + shlex.quote(match.group(3)) + " ")
                index += match.end()
                word_start = False
                continue
        if char == "\n":
            tail = "".join(out).rstrip()
            out.append(" " if tail.endswith(("|", "&&", "||", "(", "{")) or not tail else " ; ")
            index += 1
            for delimiter, strip in pending:
                body: list[str] = []
                while index < length:
                    newline = command.find("\n", index)
                    line = command[index:] if newline < 0 else command[index:newline]
                    index = length if newline < 0 else newline + 1
                    if (line.lstrip("\t") if strip else line) == delimiter:
                        break
                    body.append(line)
                bodies.append("\n".join(body))
            pending = []
            word_start = True
            continue
        out.append(char)
        word_start = char in " \t;&|()<>"
        index += 1
    if quote:
        raise Unparseable("unterminated quote")
    bodies.extend("" for _ in pending)
    return "".join(out), bodies


def tokenize(text: str) -> list[str]:
    try:
        lexer = shlex.shlex(text, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        lexer.commenters = ""
        return list(lexer)
    except ValueError as error:
        raise Unparseable(str(error)) from error


def split_segments(tokens: list[str]) -> list[list[str]]:
    segments, current = [], []
    for token in tokens:
        if token in SEPARATORS:
            if current:
                segments.append(current)
            current = []
        else:
            current.append(token)
    if current:
        segments.append(current)
    return segments


def options_and_operands(argv: list[str], takes_value: set[str] = frozenset()) -> tuple[list[str], list[str]]:
    options, operands, index, literal = [], [], 0, False
    while index < len(argv):
        word = argv[index]
        if literal or word == "-" or not word.startswith("-"):
            operands.append(word)
        elif word == "--":
            literal = True
        else:
            options.append(word)
            if word in takes_value and index + 1 < len(argv):
                options.append(argv[index + 1])
                index += 1
        index += 1
    return options, operands


class Segment:
    def __init__(self) -> None:
        self.argv: list[str] = []
        self.outputs: list[str] = []
        self.stdin: list[str] = []   # heredoc bodies and here-strings
        self.chdir: str | None = None
        self.xargs = False
        self.assigned: list[str] = []  # variable names this segment sets


def read_segment(tokens: list[str], bodies: list[str]) -> Segment:
    """Split one segment into argv, output files, and stdin text; peel wrappers."""
    segment = Segment()
    argv: list[str] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        nxt = tokens[index + 1] if index + 1 < len(tokens) else ""
        if token in REDIRECT_OUT or token == ">&":
            if nxt and not (token == ">&" and (nxt.isdigit() or nxt == "-")) and nxt not in ("/dev/null", "/dev/stdout", "/dev/stderr"):
                segment.outputs.append(nxt)
            if argv and argv[-1].isdigit():
                argv.pop()
            index += 2
            continue
        if token == "<<":
            segment.stdin.append(bodies.pop(0) if bodies else "")
            index += 2
            continue
        if token == "<<<":
            segment.stdin.append(nxt)
            index += 2
            continue
        if token in REDIRECT_IN:
            if argv and argv[-1].isdigit():
                argv.pop()
            index += 2
            continue
        argv.append(token)
        index += 1
    # Peel prefixes that run another command.
    while argv:
        head = os.path.basename(argv[0])
        if ASSIGNMENT.match(argv[0]):
            segment.assigned.append(assigned_name(argv[0]))
            argv = argv[1:]
        elif argv[0] == "!":
            argv = argv[1:]
        elif head in ("export", "declare", "typeset", "readonly", "local"):
            segment.assigned.extend(assigned_name(word) for word in argv[1:] if not word.startswith("-"))
            argv = []
        elif head in ("command", "builtin"):
            argv = argv[1:]
            if argv and argv[0] in ("-v", "-V"):
                return segment
            while argv and argv[0] == "-p":
                argv = argv[1:]
        elif head in ("nohup", "time", "sudo", "doas"):
            argv = [word for word in argv[1:]]
            while argv and argv[0].startswith("-"):
                argv = argv[1:]
        elif head == "exec":
            argv = argv[1:]
            while argv and argv[0].startswith("-"):
                argv = argv[2:] if argv[0] == "-a" else argv[1:]
        elif head == "nice":
            argv = argv[1:]
            while argv and argv[0].startswith("-"):
                argv = argv[2:] if argv[0] == "-n" else argv[1:]
        elif head == "stdbuf":
            argv = argv[1:]
            while argv and argv[0].startswith("-"):
                argv = argv[2:] if argv[0] in ("-i", "-o", "-e") else argv[1:]
        elif head == "timeout":
            argv = argv[1:]
            while argv and argv[0].startswith("-"):
                argv = argv[2:] if argv[0] in ("-s", "-k", "--signal", "--kill-after") else argv[1:]
            argv = argv[1:]  # the duration
        elif head == "env":
            argv = argv[1:]
            while argv and (argv[0].startswith("-") or ASSIGNMENT.match(argv[0])):
                word = argv[0]
                if word in ("-C", "--chdir") and len(argv) > 1:
                    segment.chdir = argv[1]
                    argv = argv[2:]
                elif word.startswith("--chdir="):
                    segment.chdir = word.split("=", 1)[1]
                    argv = argv[1:]
                elif word in ("-u", "--unset") and len(argv) > 1:
                    argv = argv[2:]
                elif word in ("-S", "--split-string") and len(argv) > 1:
                    argv = shlex.split(argv[1]) + argv[2:]
                elif word.startswith("--split-string="):
                    argv = shlex.split(word.split("=", 1)[1]) + argv[1:]
                else:
                    if ASSIGNMENT.match(word):
                        segment.assigned.append(assigned_name(word))
                    argv = argv[1:]
        elif head == "xargs":
            segment.xargs = True
            argv = argv[1:]
            while argv and argv[0].startswith("-"):
                takes = argv[0] in ("-I", "-n", "-L", "-P", "-d", "-s", "-E", "-a", "--arg-file", "--delimiter")
                argv = argv[2:] if takes else argv[1:]
        else:
            break
    segment.argv = argv
    return segment


def command_writes(argv: list[str]) -> list[tuple[str, str]]:
    """(path, file|tree) the command named by argv[0] would write. tree means the
    path's whole subtree may be deleted, moved, or have its mode changed."""
    name = os.path.basename(argv[0])
    args = argv[1:]
    if name == "tee":
        return [(path, "file") for path in options_and_operands(args)[1]]
    if name in ("cp", "install", "ln", "mv"):
        options, operands = options_and_operands(args, {"-t", "-S", "-m", "-o", "-g", "--target-directory"})
        recursive = name == "mv" or any(re.fullmatch(r"-[A-Za-z]*[rRa][A-Za-z]*|--recursive|--archive", option) for option in options)
        kind = "tree" if recursive else "file"
        destination = None
        for position, option in enumerate(options):
            if option in ("-t", "--target-directory") and position + 1 < len(options):
                destination = options[position + 1]
            elif option.startswith("--target-directory="):
                destination = option.split("=", 1)[1]
        if name == "mv":
            # A move deletes its sources and replaces its destination.
            return [(path, "tree") for path in operands + ([destination] if destination else [])]
        if destination:
            return [(destination, "tree" if recursive else "dir")]
        return [(operands[-1], kind)] if len(operands) >= 2 else [(path, kind) for path in operands]
    if name in ("rm", "rmdir", "unlink", "shred"):
        return [(path, "tree") for path in options_and_operands(args)[1]]
    if name in ("touch", "mkdir"):
        return [(path, "file") for path in options_and_operands(args, {"-m", "-d", "-r", "-t"})[1]]
    if name == "truncate":
        return [(path, "file") for path in options_and_operands(args, {"-s", "-r"})[1]]
    if name == "chmod":
        operands = [word for word in args if not CHMOD_MODE.match(word) and not re.fullmatch(r"-[RvcfhHLP]+|--[a-z-]+(=.*)?", word)]
        return [(path, "tree") for path in operands]
    if name in ("chown", "chgrp"):
        return [(path, "tree") for path in options_and_operands(args, {"--reference"})[1][1:]]
    if name == "dd":
        return [(arg[3:], "file") for arg in args if arg.startswith("of=")]
    if name in ("sed", "perl"):
        # -i, possibly bundled after switches that take no argument (sed -ni, perl -pi).
        bundle = r"-[nErsuz]*i.*" if name == "sed" else r"-[pnlaw0sStTWX]*i.*"
        in_place = any(arg.startswith("--in-place") or re.fullmatch(bundle, arg) for arg in args if arg.startswith("-"))
        if not in_place:
            return []
        options, operands = options_and_operands(args, {"-e", "-f", "--expression", "--file", "-E"})
        scripted = any(option in ("-e", "-f", "--expression", "--file", "-E") or re.fullmatch(r"-[A-Za-z]*e", option) for option in options)
        return [(path, "file") for path in (operands if scripted else operands[1:])]
    if name == "rsync":
        operands = options_and_operands(args)[1]
        return [(operands[-1], "tree")] if len(operands) >= 2 else []
    if name == "find":
        writes = any(word in ("-delete", "-fprint", "-fprintf", "-fls", "-fprint0") for word in args)
        for position, word in enumerate(args):
            # -exec runs a command per file; only a writing command writes.
            if word in ("-exec", "-execdir", "-ok", "-okdir") and position + 1 < len(args):
                executed = os.path.basename(args[position + 1])
                writes = writes or executed in WRITERS | OPAQUE_WRITERS
        if not writes:
            return []
        starts = []
        for word in args:
            if word.startswith("-") or word in ("(", "!", ")"):
                break
            starts.append(word)
        return [(path, "tree") for path in (starts or ["."])]
    if name == "git":
        index = 0
        while index < len(args) and args[index].startswith("-"):
            index += 2 if args[index] in ("-C", "-c", "--git-dir", "--work-tree") else 1
        if index >= len(args):
            return []
        sub, rest = args[index], args[index + 1:]
        if sub in ("rm", "mv", "restore", "checkout", "clean", "apply", "checkout-index", "read-tree"):
            return [(path, "tree") for path in options_and_operands(rest, {"-m", "-s", "--source", "-b", "-B"})[1]]
        return []
    return []


def analyse(command: str, cwd: str, project: Path, depth: int = 0) -> Analysis:
    """Every write a shell command would make, with the directories it would run in."""
    result = Analysis()
    if depth > 4:
        raise Unparseable("nested commands too deep")
    text, bodies = preprocess(command)
    tokens = tokenize(text)
    result.words.extend(token for token in tokens if token not in SEPARATORS)
    # Command substitutions run commands of their own.
    substitutions = re.findall(r"\$\(((?:[^()]|\([^()]*\))*)\)", text) + re.findall(r"`([^`]*)`", text)
    for body in bodies:
        result.words.extend(body.split())
    cwds: tuple[str, ...] = (cwd,)
    stack: list[tuple[str, ...]] = []

    def nested(text: str, where: tuple[str, ...]) -> None:
        for place in where:
            inner = analyse(text, place, project, depth + 1)
            result.targets.extend(inner.targets)
            result.inline.extend(inner.inline)
            result.words.extend(inner.words)
            result.opaque = result.opaque or inner.opaque
            result.refusal = result.refusal or inner.refusal

    def move(destination: str | None) -> tuple[str, ...]:
        if destination is None or destination in ("-", "~"):
            return (str(project), os.path.expanduser("~")) if destination == "~" else cwds + (str(project),)
        expanded = re.sub(r"^\$\{?HOME\}?(?=/|$)", os.path.expanduser("~"), os.path.expanduser(destination))
        if VARIABLE.search(expanded):
            return cwds + (str(project),)
        return tuple(os.path.normpath(os.path.join(place, expanded)) for place in cwds)

    for inner in substitutions:
        nested(inner, cwds)
    for tokens_of_segment in split_segments(tokens):
        segment = read_segment(tokens_of_segment, bodies)
        for name in segment.assigned:
            if guard_variable(name) and not result.refusal:
                result.refusal = f"sets the guard setting {name}"
        if segment.argv and not result.refusal:
            words = segment.argv
            if os.path.basename(words[0]) in SETTERS:
                for word in words[1:]:
                    if guard_variable(assigned_name(word)):
                        result.refusal = f"sets the guard setting {assigned_name(word)}"
            # The CLI may be run by a launcher (bash FILE, xargs, find -exec,
            # watch, a symlink): look for it at every position, not only first.
            for position, word in enumerate(words):
                if result.refusal:
                    break
                if is_harness_cli(word, cwds[0]) or os.path.basename(word) == "knowledge-trust.sh":
                    candidate = ["harness" if is_harness_cli(word, cwds[0]) else word, *words[position + 1:]]
                    command_name = human_only(candidate)
                    if command_name:
                        result.refusal = f"runs {command_name}, which is a person's to run"
                    elif segment.xargs and is_harness_cli(word, cwds[0]):
                        result.refusal = "runs the harness CLI with arguments from input, which is a person's to run"
        here = move(segment.chdir) if segment.chdir else cwds
        result.targets.extend((path, here, "file") for path in segment.outputs)
        argv = segment.argv
        if not argv:
            continue
        name = os.path.basename(argv[0])
        stem = re.sub(r"[\d.]+$", "", name)
        if name in ("cd", "pushd"):
            operands = [word for word in argv[1:] if not re.fullmatch(r"-[PLe@]+", word)]
            if name == "pushd":
                stack.append(cwds)
            cwds = move(operands[0] if operands else "~")
            continue
        if name == "popd":
            cwds = stack.pop() if stack else cwds + (str(project),)
            continue
        if segment.xargs and (name in WRITERS | OPAQUE_WRITERS or name in SHELLS or stem in INTERPRETERS):
            result.opaque = True
        if name == "eval":
            nested(" ".join(argv[1:]), here)
            continue
        if name in SHELLS:
            script = None
            for position, word in enumerate(argv[1:], 1):
                if word.startswith("-") and not word.startswith("--") and "c" in word[1:]:
                    rest = [w for w in argv[position + 1:] if not w.startswith("-")]
                    script = rest[0] if rest else ""
                    break
            if script is not None:
                nested(script, here)
            for text_in in segment.stdin:
                nested(text_in, here)
            continue
        if stem in INTERPRETERS:
            result.inline.append(" ".join(argv[1:]))
            result.inline.extend(segment.stdin)
            if name.startswith("perl"):
                result.targets.extend((path, here, kind) for path, kind in command_writes(["perl", *argv[1:]]))
            continue
        if name in OPAQUE_WRITERS:
            result.opaque = True
            continue
        if name == "git":
            for position, word in enumerate(argv[1:-1], 1):
                if word == "-C":
                    here = move(argv[position + 1])
        if name == "find" and any(word in ("-exec", "-execdir", "-ok", "-okdir") for word in argv):
            tail = argv[argv.index(next(w for w in argv if w in ("-exec", "-execdir", "-ok", "-okdir"))) + 1:]
            if tail and (os.path.basename(tail[0]) in SHELLS or re.sub(r"[\d.]+$", "", os.path.basename(tail[0])) in INTERPRETERS):
                result.opaque = True
        result.targets.extend((path, here, kind) for path, kind in command_writes(argv))
    return result


# --- judging ------------------------------------------------------------------

def expand_braces(word: str) -> list[str]:
    match = re.search(r"\{([^{}]*,[^{}]*)\}", word)
    if not match:
        return [word]
    out = []
    for part in match.group(1).split(","):
        out.extend(expand_braces(word[:match.start()] + part + word[match.end():]))
    return out[:64]


def relative(path: str, project: Path) -> str:
    resolved = os.path.realpath(path)
    try:
        rel = os.path.relpath(resolved, project)
    except ValueError:
        return resolved
    return resolved if rel == ".." or rel.startswith("../") else rel


def protected_probes(rules, rules_path) -> list[str]:
    return [probe for probe in PROBES if first_match(rules, rules_path, "path", probe)]


def judge_subject(subject: str, kind: str, project: Path, rules, rules_path) -> str | None:
    # A directory operand stands for the paths beneath it.
    directory = kind != "file" or os.path.isdir(subject if os.path.isabs(subject) else project / subject)
    for candidate in (subject, subject.rstrip("/") + "/") if directory else (subject,):
        denial = first_match(rules, rules_path, "path", candidate)
        if denial:
            return denial
    if kind == "tree":
        base = "" if subject in (".", "") else subject.rstrip("/") + "/"
        absolute = subject if os.path.isabs(subject) else str(project / subject)
        for probe in protected_probes(rules, rules_path):
            if probe.startswith(base) or (os.path.isabs(subject) and (str(project / probe)).startswith(absolute.rstrip("/") + "/")):
                return f"DENY: {subject} contains protected {probe} ({rules_path})"
    return None


def judge_target(target: str, cwds: tuple[str, ...], kind: str, project: Path, rules, rules_path) -> str | None:
    expanded = re.sub(r"^\$\{?HOME\}?(?=/|$)", os.path.expanduser("~"), os.path.expanduser(target))
    if VARIABLE.search(expanded):
        suffix = VARIABLE.split(expanded)[-1].lstrip("/")
        if suffix:
            denial = judge_subject(suffix, kind, project, rules, rules_path)
            if denial:
                return denial
            for probe in protected_probes(rules, rules_path):
                if probe.endswith("/" + suffix) or probe == suffix:
                    return f"DENY: a variable path ending in {suffix} can name protected {probe}"
        return None
    for pattern in expand_braces(expanded):
        for place in cwds:
            joined = os.path.normpath(os.path.join(place, pattern))
            if GLOB_CHARS.search(pattern):
                pattern_rel = os.path.relpath(joined, project)
                pattern_rel = joined if pattern_rel.startswith("..") else pattern_rel
                for probe in protected_probes(rules, rules_path):
                    if fnmatch.fnmatch(probe, pattern_rel) or (kind == "tree" and fnmatch.fnmatch(probe, pattern_rel + "/*")):
                        return f"DENY: {target} can match protected {probe} ({rules_path})"
                for match in glob.glob(joined)[:256]:
                    denial = judge_subject(relative(match, project), kind, project, rules, rules_path)
                    if denial:
                        return denial
                continue
            denial = judge_subject(relative(joined, project), kind, project, rules, rules_path)
            if denial:
                return denial
    return None


def mentions_protected(words: list[str], cwd: str, project: Path, rules, rules_path) -> str | None:
    """For writes that cannot be known: refuse when any word could name a protected path."""
    for word in words:
        if "/" not in word and not word.startswith("."):
            continue
        denial = judge_target(word.strip("'\""), (cwd, str(project)), "tree", project, rules, rules_path)
        if denial:
            return denial
    return None


def check_command(command: str, project: Path, cwd: str) -> str | None:
    rules_path, rules = load_rules(project)
    denial = first_match(rules, rules_path, "command", command)
    if denial:
        return denial
    try:
        analysis = analyse(command, cwd, project)
    except Unparseable:
        # Writes are unknowable: refuse if the text names a protected path at all.
        words = re.findall(r"[^\s'\";&|<>()`$]+", command)
        denial = mentions_protected(words, cwd, project, rules, rules_path)
        return f"{denial} (the command cannot be parsed)" if denial else first_match(rules, rules_path, "inline", command)
    if analysis.refusal:
        return f"DENY: the command {analysis.refusal}"
    for code in analysis.inline:
        denial = first_match(rules, rules_path, "inline", code)
        if denial:
            return denial
    for target, cwds, kind in analysis.targets:
        denial = judge_target(target, cwds, kind, project, rules, rules_path)
        if denial:
            return f"{denial} (write target {target})"
    if analysis.opaque:
        denial = mentions_protected(analysis.words, cwd, project, rules, rules_path)
        if denial:
            return f"{denial} (its writes cannot be known)"
    return None


def project_targets(command: str, project: Path, cwd: str) -> list[str]:
    """Write targets inside the project, project-relative. '?' when unknowable."""
    try:
        analysis = analyse(command, cwd, project)
    except Unparseable:
        return ["?"]
    found = ["?"] if analysis.opaque else []
    for target, cwds, _ in analysis.targets:
        expanded = os.path.expanduser(target)
        if VARIABLE.search(expanded):
            suffix = VARIABLE.split(expanded)[-1].lstrip("/")
            found.append(suffix or "?")
            continue
        for place in cwds:
            for pattern in expand_braces(expanded):
                rel = relative(os.path.join(place, pattern), project)
                if not os.path.isabs(rel):
                    found.append(rel)
    return found


def harness_call(command: str, project: Path, cwd: str) -> str:
    """pass: one plain call of this harness's bookkeeping CLI that the guard lets
    through; human: a command that is a person's to run, anywhere in the command
    (judged on the parsed argv, so quoting cannot hide it); no: anything else."""
    try:
        analysis = analyse(command, cwd, project)
        if analysis.refusal and "person's" in analysis.refusal:
            return "human"
    except Unparseable:
        if re.search(r"harness|knowledge-trust", command):
            return "no"
    if any(char in command for char in "$`<>\n\\"):
        return "no"
    try:
        tokens = tokenize(command)
    except Unparseable:
        return "no"
    separators = [token for token in tokens if token in SEPARATORS]
    segments = split_segments(tokens)
    where = cwd
    if len(segments) == 2 and separators in (["&&"], [";"]) and segments[0][0] == "cd" and len(segments[0]) == 2:
        where = os.path.normpath(os.path.join(cwd, os.path.expanduser(segments[0][1])))
        segments = segments[1:]
    elif separators or len(segments) != 1:
        return "no"
    argv = segments[0]
    if re.match(r"^[A-Za-z_]\w*=", argv[0]):
        return "no"
    executable = argv[0]
    located = os.path.join(where, executable) if "/" in executable else (shutil.which(executable) or "")
    real = os.path.realpath(located) if located else ""
    if real == os.path.realpath(project / "scripts" / "harness"):
        # Only bookkeeping skips the other checks; launch, budget, and anything
        # aimed at another session are judged like every command.
        sub, second, session = harness_subcommand(argv)
        if session or sub not in PASSABLE or (sub == "review" and second not in ("start", "done")):
            return "no"
        return "pass"
    if real == os.path.realpath(project / "scripts" / "action.sh") and len(argv) == 3 and argv[1] == "validate":
        return "pass"
    return "no"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("check", "targets", "harness-call", "rules"))
    parser.add_argument("--command", dest="shell_command")
    parser.add_argument("--path")
    parser.add_argument("--project", default=os.environ.get("HARNESS_TARGET_ROOT") or str(HARNESS_ROOT))
    parser.add_argument("--cwd")
    args = parser.parse_args()
    project = Path(args.project)
    if not project.is_dir():
        print(f"FAIL: project root does not exist: {project}")
        return 2
    project = project.resolve()
    cwd = args.cwd if args.cwd and os.path.isdir(args.cwd) else str(project)
    try:
        if args.action == "rules":
            rules_path, rules = load_rules(project)
            print(f"Rules: {rules_path}")
            for kind, pattern, _ in rules:
                print(f"{kind} {pattern}")
            return 0
        if args.action in ("targets", "harness-call"):
            if args.shell_command is None:
                parser.error(args.action + " needs --command")
            if args.action == "harness-call":
                print(harness_call(args.shell_command, project, cwd))
                return 0
            for target in project_targets(args.shell_command, project, cwd):
                print(target)
            return 0
        if (args.shell_command is None) == (args.path is None):
            print("FAIL: check needs exactly one of --command or --path.")
            return 2
        if args.shell_command is not None:
            denial = check_command(args.shell_command, project, cwd)
            kind = "command"
        else:
            rules_path, rules = load_rules(project)
            subject = relative(os.path.join(cwd, os.path.expanduser(args.path)), project)
            denial = judge_subject(subject, "file", project, rules, rules_path)
            kind = "path"
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 2
    if denial:
        print(denial)
        return 1
    print(f"OK: {kind} allowed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
