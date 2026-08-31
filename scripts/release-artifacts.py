#!/usr/bin/env python3
"""Validate, inventory, and reconcile Rookhold release artifacts.

This script is intentionally standard-library-only so the privileged publish job
can establish its exact release boundary before installing any dependencies.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
import tarfile
import zipfile
from collections.abc import Iterable, Sequence
from pathlib import Path, PurePosixPath
from typing import BinaryIO

MAX_ARCHIVE_FILES = 10_000
MAX_EXPANDED_BYTES = 512 * 1024 * 1024
COPY_CHUNK_BYTES = 1024 * 1024


def payload_names(version: str) -> tuple[str, ...]:
    require_version(version)
    return (
        "rookhold-aarch64-apple-darwin.tar.gz",
        "rookhold-sdk-" + version + ".tgz",
        "rookhold-x86_64-pc-windows-msvc.zip",
        "rookhold-x86_64-unknown-linux-musl.tar.gz",
        "rookhold_sdk-" + version + "-py3-none-any.whl",
        "rookhold_sdk-" + version + ".tar.gz",
    )


def sbom_name(version: str) -> str:
    require_version(version)
    return "rookhold-" + version + ".spdx.json"


def release_names(version: str) -> tuple[str, ...]:
    return tuple(sorted((*payload_names(version), sbom_name(version), "SHA256SUMS")))


def require_version(version: str) -> None:
    if re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", version) is None:
        raise ValueError(f"invalid release version: {version!r}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(COPY_CHUNK_BYTES):
            digest.update(chunk)
    return digest.hexdigest()


def require_exact_files(directory: Path, expected: Sequence[str]) -> None:
    if not directory.is_dir() or directory.is_symlink():
        raise ValueError(f"release directory is not a real directory: {directory}")
    actual = sorted(entry.name for entry in directory.iterdir())
    wanted = sorted(expected)
    if actual != wanted:
        missing = sorted(set(wanted) - set(actual))
        unexpected = sorted(set(actual) - set(wanted))
        raise ValueError(
            f"release artifact set differs from its allowlist; missing={missing}, "
            f"unexpected={unexpected}, count={len(actual)}, expected_count={len(wanted)}"
        )
    for name in wanted:
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(
                f"release artifact is not a regular non-symlink file: {name}"
            )
        if path.stat().st_size <= 0:
            raise ValueError(f"release artifact is empty: {name}")


def safe_member_path(raw_name: str) -> Path:
    if not raw_name or "\x00" in raw_name or "\\" in raw_name:
        raise ValueError(f"archive contains an empty or ambiguous path: {raw_name!r}")
    trimmed = raw_name.rstrip("/")
    pure = PurePosixPath(trimmed)
    if (
        not trimmed
        or pure.is_absolute()
        or any(part in {"", ".", ".."} for part in pure.parts)
    ):
        raise ValueError(f"archive contains an unsafe path: {raw_name!r}")
    if ":" in pure.parts[0]:
        raise ValueError(f"archive contains a drive-like path: {raw_name!r}")
    return Path(*pure.parts)


def copy_exact(source: BinaryIO, destination: Path, expected_size: int) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() or destination.is_symlink():
        raise ValueError(
            f"archive member collides with an existing path: {destination}"
        )
    observed = 0
    with destination.open("xb") as output:
        while chunk := source.read(COPY_CHUNK_BYTES):
            observed += len(chunk)
            if observed > expected_size:
                raise ValueError(
                    f"archive member exceeded its declared size: {destination}"
                )
            output.write(chunk)
    if observed != expected_size:
        raise ValueError(
            f"archive member size mismatch for {destination}: expected {expected_size}, got {observed}"
        )


def record_member(
    raw_name: str,
    size: int,
    seen: set[str],
    totals: list[int],
) -> Path:
    relative = safe_member_path(raw_name)
    collision_key = relative.as_posix().casefold()
    if collision_key in seen:
        raise ValueError(
            f"archive contains a duplicate or case-colliding path: {raw_name!r}"
        )
    seen.add(collision_key)
    totals[0] += 1
    totals[1] += size
    if totals[0] > MAX_ARCHIVE_FILES:
        raise ValueError(f"archive contains more than {MAX_ARCHIVE_FILES} members")
    if totals[1] > MAX_EXPANDED_BYTES:
        raise ValueError(f"archive expands beyond {MAX_EXPANDED_BYTES} bytes")
    return relative


def extract_tar(archive_path: Path, destination: Path) -> None:
    seen: set[str] = set()
    totals = [0, 0]
    with tarfile.open(archive_path, mode="r:gz") as archive:
        members = archive.getmembers()
        for member in members:
            if not (member.isdir() or member.isreg()):
                raise ValueError(
                    f"archive contains a link, device, FIFO, or unsupported member: {member.name!r}"
                )
            record_member(
                member.name, member.size if member.isreg() else 0, seen, totals
            )
        for member in members:
            relative = safe_member_path(member.name)
            target = destination / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                if target.is_symlink() or not target.is_dir():
                    raise ValueError(
                        f"archive directory collides with another member: {member.name!r}"
                    )
                continue
            source = archive.extractfile(member)
            if source is None:
                raise ValueError(
                    f"could not read regular archive member: {member.name!r}"
                )
            with source:
                copy_exact(source, target, member.size)
            os.chmod(target, member.mode & 0o777)


def extract_zip(archive_path: Path, destination: Path) -> None:
    seen: set[str] = set()
    totals = [0, 0]
    with zipfile.ZipFile(archive_path) as archive:
        members = archive.infolist()
        for member in members:
            if member.flag_bits & 0x1:
                raise ValueError(
                    f"archive contains an encrypted member: {member.filename!r}"
                )
            unix_mode = member.external_attr >> 16
            file_type = stat.S_IFMT(unix_mode)
            if file_type not in {0, stat.S_IFDIR, stat.S_IFREG}:
                raise ValueError(
                    f"archive contains a link or unsupported member: {member.filename!r}"
                )
            if member.is_dir() and file_type == stat.S_IFREG:
                raise ValueError(
                    f"archive directory has a regular-file mode: {member.filename!r}"
                )
            record_member(
                member.filename,
                0 if member.is_dir() else member.file_size,
                seen,
                totals,
            )
        for member in members:
            relative = safe_member_path(member.filename)
            target = destination / relative
            if member.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                if target.is_symlink() or not target.is_dir():
                    raise ValueError(
                        f"archive directory collides with another member: {member.filename!r}"
                    )
                continue
            with archive.open(member, mode="r") as source:
                copy_exact(source, target, member.file_size)


def required_paths(asset: str, version: str) -> tuple[str, ...]:
    if asset == "rookhold-x86_64-unknown-linux-musl.tar.gz":
        root = "rookhold-x86_64-unknown-linux-musl"
        return tuple(
            f"{root}/{name}"
            for name in (
                "rookhold",
                "coop",
                "rookhold-verify",
                "coop-verify",
                "rookhold-sandbox-init",
                "coop-sandbox-init",
                "rookhold-oci-init",
                "coop-oci-init",
                "README.md",
                "LICENSE",
                "SECURITY.md",
                "AUDIT.md",
                "CHANGELOG.md",
                "docs",
                "deploy",
                "sdks",
                "integrations",
                "scripts/build-rootfs-manifest.py",
                "scripts/smoke-gvisor.sh",
            )
        )
    if asset == "rookhold-aarch64-apple-darwin.tar.gz":
        root = "rookhold-aarch64-apple-darwin"
        return tuple(
            f"{root}/{name}"
            for name in (
                "rookhold",
                "coop",
                "rookhold-verify",
                "coop-verify",
                "README.md",
                "LICENSE",
                "SECURITY.md",
                "AUDIT.md",
                "CHANGELOG.md",
                "docs",
                "deploy",
                "sdks",
                "integrations",
                "scripts/build-rootfs-manifest.py",
                "scripts/smoke-gvisor.sh",
            )
        )
    if asset == "rookhold-x86_64-pc-windows-msvc.zip":
        root = "rookhold-x86_64-pc-windows-msvc"
        return tuple(
            f"{root}/{name}"
            for name in (
                "rookhold.exe",
                "coop.exe",
                "rookhold-verify.exe",
                "coop-verify.exe",
                "README.md",
                "LICENSE",
                "SECURITY.md",
                "AUDIT.md",
                "CHANGELOG.md",
                "docs",
                "deploy",
                "sdks",
                "integrations",
            )
        )
    if asset == "rookhold_sdk-" + version + "-py3-none-any.whl":
        return (
            "rookhold/__init__.py",
            "rookhold/py.typed",
            "rookhold_mcp.py",
            "coop/__init__.py",
            "coop/py.typed",
            "coop_mcp.py",
            f"rookhold_sdk-{version}.dist-info/METADATA",
            f"rookhold_sdk-{version}.dist-info/RECORD",
        )
    if asset == "rookhold_sdk-" + version + ".tar.gz":
        root = "rookhold_sdk-" + version
        return tuple(
            f"{root}/{name}"
            for name in (
                "rookhold.py",
                "rookhold_mcp.py",
                "coop.py",
                "coop_mcp.py",
                "py.typed",
                "coop.py.typed",
                "pyproject.toml",
                "README.md",
                "LICENSE",
                "tests",
            )
        )
    if asset == "rookhold-sdk-" + version + ".tgz":
        return (
            "package/package.json",
            "package/README.md",
            "package/LICENSE",
            "package/dist/rookhold.js",
            "package/dist/rookhold.d.ts",
            "package/dist/coop.js",
            "package/dist/coop.d.ts",
        )
    raise ValueError(f"no archive contract is defined for {asset}")


def inventory_files(root: Path) -> list[Path]:
    return sorted(
        (path for path in root.rglob("*") if path.is_file() and not path.is_symlink()),
        key=lambda path: path.relative_to(root).as_posix(),
    )


def write_checksums(path: Path, directory: Path, names: Iterable[str]) -> None:
    if path.exists() or path.is_symlink():
        raise ValueError(f"checksum output already exists: {path}")
    lines = [f"{sha256(directory / name)}  {name}\n" for name in sorted(names)]
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as handle:
        handle.writelines(lines)


def assemble(downloads: Path, dist: Path, version: str) -> None:
    containers = {
        "rookhold-sdks": (
            "rookhold-sdk-" + version + ".tgz",
            "rookhold_sdk-" + version + "-py3-none-any.whl",
            "rookhold_sdk-" + version + ".tar.gz",
        ),
        "rookhold-aarch64-apple-darwin": ("rookhold-aarch64-apple-darwin.tar.gz",),
        "rookhold-x86_64-pc-windows-msvc": ("rookhold-x86_64-pc-windows-msvc.zip",),
        "rookhold-x86_64-unknown-linux-musl": ("rookhold-x86_64-unknown-linux-musl.tar.gz",),
    }
    if not downloads.is_dir() or downloads.is_symlink():
        raise ValueError(f"artifact-download root is not a real directory: {downloads}")
    actual_containers = sorted(entry.name for entry in downloads.iterdir())
    if actual_containers != sorted(containers):
        raise ValueError(
            "downloaded workflow-artifact containers differ from the exact allowlist; "
            f"got={actual_containers!r}, expected={sorted(containers)!r}"
        )
    if dist.exists() or dist.is_symlink():
        raise ValueError(f"release directory must be freshly created: {dist}")
    dist.mkdir(parents=True)
    for container, names in containers.items():
        source = downloads / container
        require_exact_files(source, names)
        for name in names:
            destination = dist / name
            with (source / name).open("rb") as input_file:
                copy_exact(input_file, destination, (source / name).stat().st_size)
    require_exact_files(dist, payload_names(version))
    print(
        f"assembled exactly {len(payload_names(version))} payloads from "
        f"{len(containers)} named workflow artifacts"
    )


def prepare(dist: Path, output: Path, checksums: Path, version: str) -> None:
    names = payload_names(version)
    require_exact_files(dist, names)
    if output.exists() or output.is_symlink():
        raise ValueError(f"SBOM input must be freshly created: {output}")
    output.mkdir(parents=True)
    write_checksums(checksums, dist, names)

    for name in names:
        asset = dist / name
        asset_root = output / name
        payload_root = asset_root / "payload"
        payload_root.mkdir(parents=True)
        if name.endswith((".tar.gz", ".tgz")):
            extract_tar(asset, payload_root)
        elif name.endswith((".zip", ".whl")):
            extract_zip(asset, payload_root)
        else:
            raise ValueError(f"unsupported release archive: {name}")

        required_directories = {"deploy", "docs", "integrations", "sdks", "tests"}
        for required in required_paths(name, version):
            required_path = payload_root / Path(*PurePosixPath(required).parts)
            if PurePosixPath(required).name in required_directories:
                valid = required_path.is_dir() and not required_path.is_symlink()
            else:
                valid = (
                    required_path.is_file()
                    and not required_path.is_symlink()
                    and required_path.stat().st_size > 0
                )
            if not valid:
                raise ValueError(f"{name} is missing required content: {required}")
        files = inventory_files(payload_root)
        if not files:
            raise ValueError(f"{name} extracted without regular files")
        marker = {
            "asset": name,
            "sha256": sha256(asset),
            "size": asset.stat().st_size,
            "files": len(files),
        }
        marker_path = asset_root / ".rookhold-release-artifact.json"
        with marker_path.open("x", encoding="utf-8", newline="\n") as handle:
            handle.write(
                json.dumps(marker, sort_keys=True, separators=(",", ":")) + "\n"
            )
        print(
            f"inventoried {name}: {len(files)} files, {asset.stat().st_size} compressed bytes, "
            f"sha256:{marker['sha256']}"
        )


def verify_sbom(sbom: Path, version: str) -> None:
    expected_name = "rookhold-release-" + version
    if sbom.is_symlink() or not sbom.is_file() or sbom.stat().st_size <= 0:
        raise ValueError(f"SBOM is not a non-empty regular file: {sbom}")
    if sbom.stat().st_size > 16 * 1024 * 1024:
        raise ValueError("SBOM exceeds the actions/attest 16 MiB predicate limit")
    document = json.loads(sbom.read_text(encoding="utf-8"))
    if document.get("spdxVersion") != "SPDX-2.3":
        raise ValueError("release SBOM is not SPDX 2.3 JSON")
    if document.get("name") != expected_name:
        raise ValueError(f"release SBOM source name is not {expected_name!r}")
    root_packages = [
        package
        for package in document.get("packages", [])
        if package.get("name") == expected_name
        and package.get("versionInfo") == version
    ]
    if len(root_packages) != 1:
        raise ValueError(
            "release SBOM does not contain exactly one versioned document-root package"
        )
    if len(document.get("packages", [])) <= 1:
        raise ValueError("release SBOM contains no discovered payload packages")

    file_names = {
        str(item.get("fileName", "")).replace("\\", "/").lstrip("./")
        for item in document.get("files", [])
    }
    for asset in payload_names(version):
        marker = asset + "/.rookhold-release-artifact.json"
        if marker not in file_names:
            raise ValueError(
                f"release SBOM did not inventory the bound marker for {asset}"
            )
    print(
        f"verified artifact-scoped SPDX: {len(document['packages'])} packages, "
        f"{len(document.get('files', []))} files, {len(payload_names(version))} payload roots"
    )


def finalize(dist: Path, attestation_checksums: Path, version: str) -> None:
    seven = tuple(sorted((*payload_names(version), sbom_name(version))))
    require_exact_files(dist, seven)
    manifest = dist / "SHA256SUMS"
    write_checksums(manifest, dist, seven)
    require_exact_files(dist, release_names(version))
    write_checksums(attestation_checksums, dist, release_names(version))
    print(
        f"finalized exact {len(release_names(version))}-asset release with {len(seven)} manifest entries"
    )


def verify_release(dist: Path, metadata: Path, version: str, tag: str) -> None:
    expected = release_names(version)
    require_exact_files(dist, expected)
    document = json.loads(metadata.read_text(encoding="utf-8"))
    if document.get("tagName") != tag:
        raise ValueError(f"draft release tag differs from {tag!r}")
    if document.get("isDraft") is not True:
        raise ValueError("release must remain a draft during asset reconciliation")
    assets = document.get("assets")
    if not isinstance(assets, list):
        raise TypeError("release metadata does not contain an asset list")
    names = [asset.get("name") for asset in assets]
    if sorted(names) != sorted(expected) or len(names) != len(set(names)):
        raise ValueError(
            f"remote draft asset set differs from exact allowlist: got={sorted(names)!r}, "
            f"expected={sorted(expected)!r}"
        )
    by_name = {asset["name"]: asset for asset in assets}
    for name in expected:
        local = dist / name
        remote = by_name[name]
        if remote.get("state") != "uploaded":
            raise ValueError(f"remote asset is not fully uploaded: {name}")
        if remote.get("size") != local.stat().st_size:
            raise ValueError(f"remote asset size differs from local file: {name}")
        expected_digest = "sha256:" + sha256(local)
        if remote.get("digest") != expected_digest:
            raise ValueError(f"remote asset digest differs from local file: {name}")
    print(
        f"verified exact {len(expected)}-asset remote draft and all local SHA-256 digests"
    )


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    assemble_command = commands.add_parser("assemble")
    assemble_command.add_argument("--downloads", type=Path, required=True)
    assemble_command.add_argument("--dist", type=Path, required=True)
    assemble_command.add_argument("--version", required=True)

    prepare_command = commands.add_parser("prepare")
    prepare_command.add_argument("--dist", type=Path, required=True)
    prepare_command.add_argument("--output", type=Path, required=True)
    prepare_command.add_argument("--checksums", type=Path, required=True)
    prepare_command.add_argument("--version", required=True)

    verify_sbom_command = commands.add_parser("verify-sbom")
    verify_sbom_command.add_argument("--sbom", type=Path, required=True)
    verify_sbom_command.add_argument("--version", required=True)

    finalize_command = commands.add_parser("finalize")
    finalize_command.add_argument("--dist", type=Path, required=True)
    finalize_command.add_argument("--attestation-checksums", type=Path, required=True)
    finalize_command.add_argument("--version", required=True)

    verify_release_command = commands.add_parser("verify-release")
    verify_release_command.add_argument("--dist", type=Path, required=True)
    verify_release_command.add_argument("--metadata", type=Path, required=True)
    verify_release_command.add_argument("--version", required=True)
    verify_release_command.add_argument("--tag", required=True)
    return root


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "assemble":
            assemble(args.downloads, args.dist, args.version)
        elif args.command == "prepare":
            prepare(args.dist, args.output, args.checksums, args.version)
        elif args.command == "verify-sbom":
            verify_sbom(args.sbom, args.version)
        elif args.command == "finalize":
            finalize(args.dist, args.attestation_checksums, args.version)
        elif args.command == "verify-release":
            verify_release(args.dist, args.metadata, args.version, args.tag)
        else:
            raise AssertionError(f"unhandled command: {args.command}")
    except (
        OSError,
        TypeError,
        ValueError,
        json.JSONDecodeError,
        tarfile.TarError,
        zipfile.BadZipFile,
    ) as error:
        print(f"release artifact verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
