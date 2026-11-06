#!/usr/bin/env python3
"""Validate explicitly opted-in behavioral verification definitions without running them."""
import argparse
import json
import re
import sys
from pathlib import Path

MAX_BYTES = 262144
ID = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


class Invalid(ValueError):
    pass


def load_json(path):
    if path.stat().st_size > MAX_BYTES:
        raise Invalid(f"{path.name}: file exceeds {MAX_BYTES} bytes")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (ValueError, UnicodeError) as exc:
        raise Invalid(f"{path.name}: invalid JSON: {exc}") from exc


def document(root, value):
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise Invalid("document path must be a nonempty relative path")
    path = Path(value)
    if path.is_absolute() or any(part in (".", "..") for part in value.split("/")):
        raise Invalid(f"unsafe document path: {value}")
    resolved = (root / path).resolve()
    if not resolved.is_relative_to(root) or not resolved.is_file():
        raise Invalid(f"missing or unsafe document: {value}")
    if resolved.stat().st_size > MAX_BYTES:
        raise Invalid(f"document exceeds {MAX_BYTES} bytes: {value}")
    return resolved


def validate(project):
    root = Path(project).resolve(strict=True)
    manifest = root / ".harness-verification.json"
    if not manifest.exists() and not manifest.is_symlink():
        return "not enabled"
    if not manifest.resolve().is_relative_to(root) or not manifest.is_file():
        raise Invalid("manifest is missing, not a file, or escapes project")
    data = load_json(manifest)
    if not isinstance(data, dict) or not isinstance(data.get("enabled"), bool):
        raise Invalid("manifest requires boolean enabled")
    if not data["enabled"]:
        return "disabled"
    if set(data) != {"version", "enabled", "guide", "index", "features"} or type(data["version"]) is not int or data["version"] != 1:
        raise Invalid("enabled manifest requires version 1, guide, index and features")
    document(root, data["guide"])
    index_path = document(root, data["index"])
    index = index_path.read_text(encoding="utf-8")
    features = data["features"]
    if not isinstance(features, list) or not features or len(features) > 100:
        raise Invalid("features must contain 1 to 100 entries")
    seen = set()
    for feature in features:
        if not isinstance(feature, dict) or set(feature) != {"id", "document", "references"}:
            raise Invalid("feature requires id, document and references")
        name = feature["id"]
        if not isinstance(name, str) or not ID.fullmatch(name) or name in seen:
            raise Invalid(f"invalid or duplicate feature ID: {name}")
        seen.add(name)
        feature_path = document(root, feature["document"])
        links = re.findall(r"\]\(([^)#]+)(?:#[^)]*)?\)", index)
        if not any((index_path.parent / link).resolve() == feature_path for link in links if not re.match(r"[a-zA-Z][a-zA-Z0-9+.-]*:", link)):
            raise Invalid(f"feature {name}: index does not link its document")
        refs = feature["references"]
        if not isinstance(refs, list) or not refs or len(refs) > 100:
            raise Invalid(f"feature {name}: references must contain 1 to 100 entries")
        for ref in refs:
            if not isinstance(ref, dict) or set(ref) != {"path", "id"} or not isinstance(ref["id"], str) or not ref["id"] or len(ref["id"]) > 128:
                raise Invalid(f"feature {name}: invalid inventory reference")
            text = document(root, ref["path"]).read_text(encoding="utf-8")
            if not re.search(r"(?<![\w-])" + re.escape(ref["id"]) + r"(?![\w-])", text):
                raise Invalid(f"feature {name}: inventory ID {ref['id']} not found in {ref['path']}")
    return f"validated {len(seen)} features"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["validate"])
    parser.add_argument("--project", required=True)
    args = parser.parse_args()
    try:
        print(validate(args.project))
        return 0
    except (Invalid, OSError, UnicodeError, ValueError) as exc:
        print(f"behavior validation: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
