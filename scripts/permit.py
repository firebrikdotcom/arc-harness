#!/usr/bin/env python3
"""Denylist evaluation for shell commands and write paths.

Rules come from the project's .harness-denylist or schemas/denylist.default:

  command <regex>  matches the whole command text (destructive actions such as rm -rf /)
  path <regex>     matches a write target, relative to the project root when inside it
  inline <regex>   matches inline interpreter code (python -c, node -e, ...) and any
                   command that cannot be parsed, where write targets are unknowable

A shell command is parsed into segments, and every file it would write (redirections,
tee, cp, mv, rm, sed -i, dd of=, ...) is checked against the path rules. Reading or
mentioning a protected file is allowed; writing it is not.

  permit.py check --command STRING [--project PATH] [--cwd PATH]
  permit.py check --path PATH [--project PATH]
  permit.py targets --command STRING [--project PATH] [--cwd PATH]
  permit.py rules [--project PATH]

Exit 0 allowed, 1 denied, 2 usage or environment error.
"""
from __future__ import annotations

import argparse
import os
import re
import shlex
import sys
from pathlib import Path

HARNESS_ROOT = Path(__file__).resolve().parent.parent
SEPARATORS = {";", "&&", "||", "|", "&", "|&", ";;", "(", ")", "{", "}"}
REDIRECT_OUT = {">", ">>", ">|", "&>", "&>>"}
REDIRECT_IN = {"<", "<<", "<<<", "<<-"}
WRAPPERS = {"command", "exec", "nohup", "time", "nice", "builtin", "stdbuf"}
SHELLS = {"sh", "bash", "zsh", "dash", "ksh"}
INTERPRETERS = {"python", "python3", "node", "perl", "ruby", "php"}
VARIABLE = re.compile(r"\$\{[^}]*\}|\$\([^)]*\)|\$\w+|`[^`]*`")
HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_][\w.-]*)\1")


class Unparseable(Exception):
    pass


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


def logical_lines(command: str) -> str:
    """Drop heredoc bodies and join physical lines into one ;-separated command."""
    out: list[str] = []
    pending: list[tuple[str, bool]] = []
    lines = command.split("\n")
    index = 0
    while index < len(lines):
        line = lines[index]
        index += 1
        if pending:
            delimiter, strip = pending[0]
            if (line.lstrip("\t") if strip else line) == delimiter:
                pending.pop(0)
            continue
        for match in HEREDOC.finditer(line):
            pending.append((match.group(2), "<<-" in match.group(0)))
        out.append(line)
    joined = ""
    for line in out:
        stripped = line.rstrip()
        if stripped.endswith("\\"):
            joined += stripped[:-1] + " "
        elif re.search(r"(\|\||&&|\|)\s*$", stripped):
            joined += stripped + " "
        else:
            joined += stripped + " ; "
    return joined


def tokenize(command: str) -> list[str]:
    try:
        lexer = shlex.shlex(logical_lines(command), posix=True, punctuation_chars=True)
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


def command_writes(argv: list[str]) -> list[str]:
    """Files the command named by argv[0] would write."""
    name = os.path.basename(argv[0])
    args = argv[1:]
    if name == "tee":
        return options_and_operands(args)[1]
    if name in ("cp", "mv", "install", "ln"):
        options, operands = options_and_operands(args, {"-t", "-S", "-m", "-o", "-g"})
        for position, option in enumerate(options):
            if option == "-t" and position + 1 < len(options):
                return [options[position + 1]]
            if option.startswith("--target-directory="):
                return [option.split("=", 1)[1]]
        return operands[-1:] if len(operands) >= 2 else operands
    if name in ("rm", "unlink", "rmdir", "shred", "touch", "mkdir"):
        return options_and_operands(args, {"-m"})[1]
    if name == "truncate":
        return options_and_operands(args, {"-s", "-r"})[1]
    if name in ("chmod", "chown", "chgrp"):
        return options_and_operands(args, {"--reference"})[1][1:]
    if name == "dd":
        return [arg[3:] for arg in args if arg.startswith("of=")]
    if name in ("sed", "perl"):
        in_place = any(arg == "-i" or arg.startswith("-i") or arg.startswith("--in-place") or
                       (name == "perl" and re.fullmatch(r"-\w*i\w*", arg)) for arg in args)
        if not in_place:
            return []
        options, operands = options_and_operands(args, {"-e", "-f", "--expression", "--file"})
        scripted = any(option in ("-e", "-f", "--expression", "--file") for option in options)
        return operands if scripted else operands[1:]
    return []


def analyse(command: str, cwd: str, depth: int = 0) -> tuple[list[tuple[str, str]], list[str]]:
    """Return ([(target, cwd), ...], [inline code, ...]) for one shell command."""
    if depth > 3:
        raise Unparseable("nested shell too deep")
    targets: list[tuple[str, str]] = []
    inline: list[str] = []
    for segment in split_segments(tokenize(command)):
        argv: list[str] = []
        index = 0
        while index < len(segment):
            token = segment[index]
            if token in REDIRECT_OUT or token == ">&":
                if index + 1 < len(segment):
                    target = segment[index + 1]
                    if not (token == ">&" and (target.isdigit() or target == "-")) and target not in ("/dev/null", "/dev/stdout", "/dev/stderr"):
                        targets.append((target, cwd))
                    if argv and argv[-1].isdigit():
                        argv.pop()
                index += 2
                continue
            if token in REDIRECT_IN or token == "<&":
                if argv and argv[-1].isdigit():
                    argv.pop()
                index += 2
                continue
            argv.append(token)
            index += 1
        while argv and (re.match(r"^[A-Za-z_]\w*=", argv[0]) or os.path.basename(argv[0]) in WRAPPERS or argv[0] == "env"):
            if argv[0] == "env":
                argv = argv[1:]
                while argv and argv[0].startswith("-"):
                    argv = argv[1:]
                continue
            argv = argv[1:]
        if argv and os.path.basename(argv[0]) == "timeout":
            argv = options_and_operands(argv[1:])[1][1:] if len(argv) > 2 else []
        if not argv:
            continue
        name = os.path.basename(argv[0])
        if name == "cd":
            destination = argv[1] if len(argv) > 1 else "~"
            if not VARIABLE.search(destination):
                cwd = os.path.normpath(os.path.join(cwd, os.path.expanduser(destination)))
            continue
        if name in SHELLS and "-c" in argv[1:]:
            position = argv.index("-c")
            if position + 1 < len(argv):
                nested_targets, nested_inline = analyse(argv[position + 1], cwd, depth + 1)
                targets.extend(nested_targets)
                inline.extend(nested_inline)
            continue
        if re.sub(r"[\d.]+$", "", name) in INTERPRETERS:
            for position, word in enumerate(argv[1:-1], 1):
                if word in ("-c", "-e", "-E", "--eval", "-r"):
                    inline.append(argv[position + 1])
            continue
        targets.extend((target, cwd) for target in command_writes(argv))
    return targets, inline


def subjects(target: str, cwd: str, project: Path) -> list[str]:
    """Rule subjects for one write target: project-relative inside the project, absolute
    outside it. A target built from a variable is judged by its literal suffix too."""
    expanded = re.sub(r"^\$\{?HOME\}?(?=/|$)", os.environ.get("HOME", "~"), os.path.expanduser(target))
    if VARIABLE.search(expanded):
        suffix = VARIABLE.split(expanded)[-1].lstrip("/")
        return [suffix, expanded] if suffix else [expanded]
    path = Path(os.path.realpath(os.path.join(cwd, expanded)))
    try:
        return [str(path.relative_to(project))]
    except ValueError:
        return [str(path)]


def check_command(command: str, project: Path, cwd: str) -> str | None:
    rules_path, rules = load_rules(project)
    denial = first_match(rules, rules_path, "command", command)
    if denial:
        return denial
    try:
        targets, inline = analyse(command, cwd)
    except Unparseable:
        return first_match(rules, rules_path, "inline", command)
    for code in inline:
        denial = first_match(rules, rules_path, "inline", code)
        if denial:
            return denial
    for target, target_cwd in targets:
        for subject in subjects(target, target_cwd, project):
            denial = first_match(rules, rules_path, "path", subject)
            if denial:
                return f"{denial} (write target {target})"
    return None


def project_targets(command: str, project: Path, cwd: str) -> list[str]:
    """Write targets inside the project, project-relative. Unparseable commands report '?'."""
    try:
        targets, _ = analyse(command, cwd)
    except Unparseable:
        return ["?"]
    found = []
    for target, target_cwd in targets:
        expanded = os.path.expanduser(target)
        if VARIABLE.search(expanded):
            continue
        path = Path(os.path.realpath(os.path.join(target_cwd, expanded)))
        try:
            found.append(str(path.relative_to(project)))
        except ValueError:
            pass
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("check", "targets", "rules"))
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
        if args.action == "targets":
            if args.shell_command is None:
                parser.error("targets needs --command")
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
            subject = args.path
            if os.path.isabs(subject):
                resolved = Path(os.path.realpath(subject))
                try:
                    subject = str(resolved.relative_to(project))
                except ValueError:
                    subject = str(resolved)
            denial = first_match(rules, rules_path, "path", subject)
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
