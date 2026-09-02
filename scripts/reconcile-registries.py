#!/usr/bin/env python3
"""Reconcile public registry packages against exact local release bytes."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import time
from pathlib import Path
from typing import Any
from urllib.error import HTTPError
from urllib.parse import quote, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

MAX_METADATA_BYTES = 4 * 1024 * 1024
MAX_PACKAGE_BYTES = 64 * 1024 * 1024
USER_AGENT = "rookhold-release-reconciler/0.8"


class RejectRedirects(HTTPRedirectHandler):
    """Prevent a registry response from connecting to an unvalidated target."""

    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        return None


OPEN_URL = build_opener(RejectRedirects()).open


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def require_version(version: str) -> None:
    require(
        re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", version)
        is not None,
        f"invalid release version: {version!r}",
    )


def sha256_bytes(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(
    url: str,
    limit: int,
    *,
    allow_missing: bool = False,
    expected_host: str,
) -> bytes | None:
    request = Request(url, headers={"Accept": "application/json", "User-Agent": USER_AGENT})
    try:
        response = OPEN_URL(request, timeout=30)
    except HTTPError as error:
        if allow_missing and error.code == 404:
            return None
        raise ValueError(f"registry request failed with HTTP {error.code}: {url}") from error
    with response:
        final_url = response.geturl()
        final = urlsplit(final_url)
        require(
            final.scheme == "https"
            and final.hostname == expected_host
            and final.username is None
            and final.password is None,
            f"registry response left its trusted HTTPS origin: {final_url}",
        )
        content = response.read(limit + 1)
    require(len(content) <= limit, f"registry response exceeded {limit} bytes: {url}")
    return content


def fetch_json(
    url: str, *, allow_missing: bool = False, expected_host: str
) -> dict[str, Any] | None:
    content = fetch(
        url,
        MAX_METADATA_BYTES,
        allow_missing=allow_missing,
        expected_host=expected_host,
    )
    if content is None:
        return None
    try:
        value = json.loads(content, object_pairs_hook=reject_duplicate_members)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"registry returned malformed JSON: {url}") from error
    require(isinstance(value, dict), f"registry returned a non-object: {url}")
    return value


def reject_duplicate_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, member in pairs:
        require(key not in value, f"registry JSON duplicates member: {key}")
        value[key] = member
    return value


def required_file(path: Path) -> Path:
    require(path.is_file() and not path.is_symlink(), f"local package is missing: {path}")
    require(path.stat().st_size > 0, f"local package is empty: {path}")
    return path


def pypi_state(version: str, dist: Path) -> str:
    expected_paths = {
        f"rookhold-{version}-py3-none-any.whl": required_file(
            dist / f"rookhold-{version}-py3-none-any.whl"
        ),
        f"rookhold-{version}.tar.gz": required_file(dist / f"rookhold-{version}.tar.gz"),
    }
    metadata = fetch_json(
        f"https://pypi.org/pypi/rookhold/{quote(version, safe='')}/json",
        allow_missing=True,
        expected_host="pypi.org",
    )
    if metadata is None:
        return "missing"
    urls = metadata.get("urls")
    require(isinstance(urls, list), "PyPI metadata omits release files")
    remote: dict[str, str] = {}
    for item in urls:
        require(isinstance(item, dict), "PyPI metadata contains a malformed file")
        filename = item.get("filename")
        digests = item.get("digests")
        require(isinstance(filename, str), "PyPI file omits its filename")
        require(isinstance(digests, dict), f"PyPI file omits digests: {filename}")
        digest = digests.get("sha256")
        require(
            isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest) is not None,
            f"PyPI file has an invalid SHA-256: {filename}",
        )
        require(filename not in remote, f"PyPI metadata duplicates a filename: {filename}")
        remote[filename] = digest
    require(
        set(remote) == set(expected_paths),
        f"PyPI v{version} file set differs: remote={sorted(remote)}",
    )
    for filename, path in expected_paths.items():
        require(
            remote[filename] == sha256_file(path),
            f"PyPI v{version} digest differs for {filename}",
        )
    return "exact"


def npm_state(version: str, dist: Path) -> str:
    local = required_file(dist / f"rookhold-{version}.tgz")
    metadata = fetch_json(
        f"https://registry.npmjs.org/rookhold/{quote(version, safe='')}",
        allow_missing=True,
        expected_host="registry.npmjs.org",
    )
    if metadata is None:
        return "missing"
    require(metadata.get("name") == "rookhold", "npm metadata returned the wrong package")
    require(metadata.get("version") == version, "npm metadata returned the wrong version")
    distribution = metadata.get("dist")
    require(isinstance(distribution, dict), "npm metadata omits dist")
    tarball = distribution.get("tarball")
    require(isinstance(tarball, str), "npm metadata omits the tarball URL")
    parsed = urlsplit(tarball)
    require(
        parsed.scheme == "https"
        and parsed.hostname == "registry.npmjs.org"
        and parsed.username is None
        and parsed.password is None
        and parsed.query == ""
        and parsed.fragment == "",
        "npm tarball URL is outside the public registry",
    )
    remote = fetch(tarball, MAX_PACKAGE_BYTES, expected_host="registry.npmjs.org")
    assert remote is not None
    require(
        sha256_bytes(remote) == sha256_file(local),
        f"npm rookhold@{version} tarball digest differs from the gated package",
    )
    return "exact"


def registry_state(registry: str, version: str, dist: Path) -> str:
    require_version(version)
    if registry == "pypi":
        return pypi_state(version, dist)
    if registry == "npm":
        return npm_state(version, dist)
    raise ValueError(f"unsupported registry: {registry}")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subparsers = root.add_subparsers(dest="command", required=True)
    for command in ["status", "verify"]:
        child = subparsers.add_parser(command)
        child.add_argument("--registry", choices=["pypi", "npm"], required=True)
        child.add_argument("--version", required=True)
        child.add_argument("--dist", type=Path, required=True)
        if command == "verify":
            child.add_argument("--attempts", type=int, default=12)
            child.add_argument("--delay-seconds", type=float, default=5.0)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "status":
            print(registry_state(args.registry, args.version, args.dist))
            return 0
        require(args.attempts > 0, "verify attempts must be positive")
        require(args.delay_seconds >= 0, "verify delay must be non-negative")
        for attempt in range(1, args.attempts + 1):
            state = registry_state(args.registry, args.version, args.dist)
            if state == "exact":
                print(f"{args.registry} v{args.version} matches the gated package bytes")
                return 0
            if attempt < args.attempts:
                time.sleep(args.delay_seconds)
        raise ValueError(
            f"{args.registry} v{args.version} remained missing after {args.attempts} checks"
        )
    except (OSError, ValueError) as error:
        print(f"registry reconciliation failed: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
