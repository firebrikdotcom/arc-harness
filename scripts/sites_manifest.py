#!/usr/bin/env python3
"""File manifests and zip packing for `harness sites publish`.

  sites_manifest.py manifest PATH        JSON {path: {bytes, sha256}} that PATH publishes
  sites_manifest.py pack DIR OUT.zip     zip DIR for upload, hidden files left out

PATH is an .html file (published as index.html), a .zip, or a directory. The
zip rules mirror Firebrik Sites' extraction: directory entries and bare
dotfiles (".nojekyll") are skipped, and a single top-level folder with no
files beside it is unwrapped.
"""
from __future__ import annotations

import hashlib
import json
import sys
import zipfile
from pathlib import Path, PurePosixPath


def entry(data: bytes) -> dict:
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def directory_files(root: Path) -> list[tuple[str, Path]]:
    files = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if path.is_symlink():
            raise SystemExit(f"FAIL: symbolic links cannot be published: {relative}")
        if path.is_file() and not any(part.startswith(".") for part in relative.parts):
            files.append((relative.as_posix(), path))
    return files


def zip_manifest(path: Path) -> dict:
    entries = {}
    with zipfile.ZipFile(path) as archive:
        for info in archive.infolist():
            name = info.filename
            if name.endswith("/"):
                continue
            base = PurePosixPath(name).name
            # PHP's pathinfo gives ".nojekyll" an empty filename: Sites skips those.
            if base.startswith(".") and "." not in base[1:]:
                continue
            entries[name] = entry(archive.read(info))
    tops = {name.split("/", 1)[0] for name in entries}
    if entries and len(tops) == 1 and all("/" in name for name in entries):
        prefix = next(iter(tops)) + "/"
        entries = {name[len(prefix):]: value for name, value in entries.items()}
    return dict(sorted(entries.items()))


def manifest(path: Path) -> dict:
    if path.is_dir():
        return {name: entry(file.read_bytes()) for name, file in directory_files(path)}
    if zipfile.is_zipfile(path):
        return zip_manifest(path)
    return {"index.html": entry(path.read_bytes())}


def pack(root: Path, out: Path) -> None:
    files = directory_files(root)
    if not any(name == "index.html" for name, _ in files):
        raise SystemExit(f"FAIL: {root} has no index.html at its root")
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, file in files:
            archive.write(file, name)


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[0] == "manifest":
        print(json.dumps(manifest(Path(argv[1]))))
        return 0
    if len(argv) == 3 and argv[0] == "pack":
        pack(Path(argv[1]), Path(argv[2]))
        return 0
    print(__doc__.strip(), file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
