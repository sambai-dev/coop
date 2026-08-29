#!/usr/bin/env python3
"""Build Coop's canonical, content-complete trusted-rootfs manifest."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
import sys

MANIFEST = ".coop-rootfs.manifest"
TEMPORARY = ".coop-rootfs.manifest.tmp"


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(64 * 1024):
            value.update(chunk)
    return value.hexdigest()


def text(value: str, label: str) -> str:
    try:
        value.encode("utf-8", "strict")
    except UnicodeEncodeError as error:
        raise SystemExit(f"rootfs {label} is not UTF-8: {error}") from error
    if "\x00" in value or "\n" in value or "\r" in value:
        raise SystemExit(f"rootfs {label} contains a forbidden control character")
    return value


def entry(root: Path, path: Path) -> dict[str, object]:
    metadata = path.lstat()
    relative = "." if path == root else path.relative_to(root).as_posix()
    result: dict[str, object] = {
        "gid": metadata.st_gid,
        "mode": stat.S_IMODE(metadata.st_mode),
        "path": text(relative, "path"),
        "uid": metadata.st_uid,
    }
    if stat.S_ISDIR(metadata.st_mode):
        result["kind"] = "directory"
    elif stat.S_ISREG(metadata.st_mode):
        result.update(kind="file", sha256=digest(path), size=metadata.st_size)
    elif stat.S_ISLNK(metadata.st_mode):
        result.update(kind="symlink", target=text(os.readlink(path), "symlink target"))
    elif stat.S_ISCHR(metadata.st_mode):
        result.update(kind="character", rdev=metadata.st_rdev)
    elif stat.S_ISBLK(metadata.st_mode):
        result.update(kind="block", rdev=metadata.st_rdev)
    else:
        raise SystemExit(f"unsupported socket/FIFO/rootfs entry: {relative}")
    return result


def walk(root: Path, path: Path, root_device: int) -> list[dict[str, object]]:
    if path != root and path.lstat().st_dev != root_device:
        raise SystemExit(f"rootfs must not cross a mounted filesystem: {path}")
    values = [entry(root, path)]
    if not path.is_dir() or path.is_symlink():
        return values
    children = sorted(
        (
            child
            for child in path.iterdir()
            if not (path == root and child.name in {MANIFEST, TEMPORARY})
        ),
        key=lambda child: os.fsencode(child.name),
    )
    for child in children:
        values.extend(walk(root, child, root_device))
    return values


def main() -> None:
    arguments = list(sys.argv[1:])
    allow_root = "--allow-root" in arguments
    remove_self = "--remove-self" in arguments
    arguments = [value for value in arguments if value not in {"--allow-root", "--remove-self"}]
    if len(arguments) != 1:
        raise SystemExit(
            "usage: build-rootfs-manifest.py [--allow-root] [--remove-self] ROOTFS"
        )
    root = Path(arguments[0]).resolve(strict=True)
    if not root.is_dir() or (root == Path("/") and not allow_root):
        raise SystemExit("ROOTFS must be a dedicated directory")
    if remove_self:
        Path(__file__).unlink()
    output = root / MANIFEST
    temporary = root / TEMPORARY
    if temporary.exists() or temporary.is_symlink():
        raise SystemExit(f"refusing existing temporary manifest: {temporary}")
    payload = b"".join(
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True).encode()
        + b"\n"
        for value in walk(root, root, root.lstat().st_dev)
    )
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
    try:
        with os.fdopen(descriptor, "wb", closefd=True) as target:
            target.write(payload)
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, output)
        os.chmod(output, 0o444)
        directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if temporary.exists():
            temporary.unlink()
    print(hashlib.sha256(payload).hexdigest())


if __name__ == "__main__":
    main()
