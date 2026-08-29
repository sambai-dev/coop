#!/usr/bin/env python3
"""Fail when release metadata, documentation, or delivery policy drifts.

The checker intentionally uses only the Python standard library so it can run
in a fresh checkout before language dependencies are installed.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def toml_string(relative: str, section: str, key: str) -> str:
    """Read one quoted TOML value without adding a Python-version dependency."""
    current = ""
    for raw_line in read(relative).splitlines():
        line = raw_line.strip()
        section_match = re.fullmatch(r"\[([^]]+)\]", line)
        if section_match:
            current = section_match.group(1)
            continue
        if current != section:
            continue
        value_match = re.fullmatch(rf"{re.escape(key)}\s*=\s*\"([^\"]+)\"", line)
        if value_match:
            return value_match.group(1)
    raise KeyError(f"missing [{section}] {key} in {relative}")


def check_versions() -> str:
    version = toml_string("Cargo.toml", "workspace.package", "version")
    require(version == "0.3.0", f"unexpected workspace release version: {version}")

    toolchain = toml_string("rust-toolchain.toml", "toolchain", "channel")
    rust_version = toml_string("Cargo.toml", "workspace.package", "rust-version")
    require(toolchain == f"{rust_version}.0", "Cargo and rust-toolchain Rust versions differ")
    root_manifest = read("Cargo.toml")
    lockfile = read("Cargo.lock")
    for crate in ["coop-types", "coop-store", "coop-exec", "coop-server"]:
        manifest = read(f"crates/{crate}/Cargo.toml")
        require(
            "version.workspace = true" in manifest,
            f"{crate} does not inherit the workspace release version",
        )
        require(
            re.search(
                rf'\[\[package\]\]\s+name = "{re.escape(crate)}"\s+version = "{re.escape(version)}"',
                lockfile,
            )
            is not None,
            f"Cargo.lock does not record {crate} {version}",
        )
    for dependency in ["coop-types", "coop-store", "coop-exec"]:
        require(
            re.search(
                rf'^{re.escape(dependency)}\s*=\s*\{{[^\n}}]*version\s*=\s*"{re.escape(version)}"',
                root_manifest,
                re.MULTILINE,
            )
            is not None,
            f"workspace dependency {dependency} does not require {version}",
        )

    python_version = toml_string("sdks/python/pyproject.toml", "project", "version")
    typescript_package = json.loads(read("sdks/typescript/package.json"))
    typescript_version = typescript_package["version"]
    typescript_scripts = typescript_package.get("scripts", {})
    require(
        typescript_scripts.get("prepack") == "npm run build",
        "TypeScript package must build its ignored dist entrypoints before packing",
    )
    require(
        "pack-smoke" in typescript_scripts,
        "TypeScript package lacks a packed-artifact consumer smoke",
    )
    for label, candidate in [
        ("Python SDK", python_version),
        ("TypeScript SDK", typescript_version),
    ]:
        require(candidate == version, f"{label} version {candidate} differs from {version}")
    require(
        f'__version__ = "{version}"' in read("sdks/python/coop.py"),
        "Python module version differs from its package metadata",
    )
    python_lock = read("sdks/python/uv.lock")
    locked_python_project = re.search(
        r'\[\[package\]\]\s+name = "coop-sdk"\s+version = "([^"]+)"',
        python_lock,
    )
    require(
        locked_python_project is not None
        and locked_python_project.group(1) == python_version,
        "Python uv.lock project version differs from pyproject.toml",
    )
    typescript_lock = json.loads(read("sdks/typescript/package-lock.json"))
    lock_root = typescript_lock["packages"][""]
    for candidate in [typescript_lock, lock_root]:
        require(
            candidate["name"] == typescript_package["name"],
            "TypeScript lockfile name differs from package.json",
        )
        require(candidate["version"] == version, "TypeScript lockfile version differs from package.json")

    require(
        f"ARG VERSION={version}" in read("Dockerfile"),
        "Dockerfile default version differs from the workspace",
    )
    ci_workflow = read(".github/workflows/ci.yml")
    require(
        "version=$(awk" in ci_workflow
        and '--build-arg "VERSION=$version"' in ci_workflow,
        "CI container build does not derive its OCI version from Cargo.toml",
    )
    require(
        f"FROM rust:{toolchain}-" in read("Dockerfile"),
        "Docker build toolchain differs from rust-toolchain.toml",
    )
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        for configured in re.findall(r"^\s*toolchain:\s*([^\s#]+)", read(workflow), re.MULTILINE):
            require(
                configured == toolchain,
                f"{workflow} installs Rust {configured}, expected {toolchain}",
            )
    require(
        f'VERSION: "{version}"' in read("docker-compose.yml"),
        "Compose build version differs from the workspace",
    )
    require(f"## {version}" in read("CHANGELOG.md"), "CHANGELOG release heading is missing")
    release_workflow = read(".github/workflows/release.yml")
    require(
        'grep -Eqi "^## ${version}.*unreleased" CHANGELOG.md' in release_workflow,
        "release workflow does not reject an unfinalized changelog",
    )
    require(
        "[v${version}](https://github.com/sambai-dev/coop/releases/tag/v${version})"
        in release_workflow,
        "release workflow does not require the matching README release link",
    )
    require(
        'series="${version%.*}.x"' in release_workflow
        and "| tagged \\`${series}\\` releases | Supported |" in release_workflow,
        "release workflow does not require the matching supported-version line",
    )
    require(
        f"[v{version}](https://github.com/sambai-dev/coop/releases/tag/v{version})"
        in read("README.md"),
        "README current-release link differs from the workspace version",
    )
    series = version.rsplit(".", 1)[0] + ".x"
    require(
        f"| tagged `{series}` releases | Supported |" in read("SECURITY.md"),
        "SECURITY supported-version line differs from the workspace version",
    )
    return version


def check_markdown_links() -> int:
    markdown_files = [
        *(ROOT / name for name in ["README.md", "SECURITY.md", "AUDIT.md", "CHANGELOG.md"]),
        *(ROOT / "docs").glob("*.md"),
        *(ROOT / "integrations").glob("**/*.md"),
        ROOT / "sdks" / "python" / "README.md",
        ROOT / "sdks" / "typescript" / "README.md",
    ]
    checked = 0
    failures: list[str] = []
    link_pattern = re.compile(r"(?<!!)\[[^\]]+\]\(([^)]+)\)")
    for document in markdown_files:
        for raw_target in link_pattern.findall(document.read_text(encoding="utf-8")):
            target = raw_target.strip().strip("<>")
            parsed = urlsplit(target)
            if parsed.scheme or target.startswith(("#", "/")):
                continue
            relative = unquote(parsed.path)
            if not relative:
                continue
            checked += 1
            resolved = (document.parent / relative).resolve()
            if not resolved.exists():
                failures.append(f"{document.relative_to(ROOT)} -> {target}")
    require(not failures, "broken local Markdown links:\n  " + "\n  ".join(failures))
    return checked


def check_pins_and_packaging() -> int:
    workflows = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
    action_count = 0
    for workflow in workflows:
        for line_number, line in enumerate(workflow.read_text(encoding="utf-8").splitlines(), 1):
            match = re.match(r"\s*uses:\s*([^\s#]+)", line)
            if not match or match.group(1).startswith("./"):
                continue
            action_count += 1
            require(
                re.search(r"@[0-9a-f]{40}$", match.group(1)) is not None,
                f"{workflow.relative_to(ROOT)}:{line_number} action is not pinned to a full SHA",
            )

    dockerfile = read("Dockerfile")
    from_lines = [line for line in dockerfile.splitlines() if line.startswith("FROM ")]
    require(from_lines, "Dockerfile has no base images")
    for line in from_lines:
        require(
            re.search(r"@sha256:[0-9a-f]{64}(?:\s+AS\s+\S+)?$", line, re.IGNORECASE)
            is not None
            or re.search(r"^FROM\s+[a-z][a-z0-9-]*\s+AS\s+\S+$", line, re.IGNORECASE)
            is not None,
            f"Docker base is neither digest-pinned nor an earlier named stage: {line}",
        )

    for required in [
        "cargo build --locked --release -p coop-server -p coop-exec --bins",
        "coop-sandbox-init /usr/local/bin/coop-sandbox-init",
        "COOP_ROOTFS=/opt/coop/rootfs",
        "COOP_SANDBOX_HELPER=/usr/local/bin/coop-sandbox-init",
        "install -d -o root -g root -m 0700 /data /var/lib/coop/jobs /opt/coop",
        "install -d -o root -g root -m 0755 /opt/coop/rootfs",
        'test "$(dpkg --print-architecture)" = amd64',
    ]:
        require(required in dockerfile, f"Docker helper/rootfs contract missing: {required}")

    dockerignore_lines = {
        line.strip()
        for line in read(".dockerignore").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }
    for required in ["target", "target-*"]:
        require(required in dockerignore_lines, f"Docker context does not exclude {required}")

    snapshot_match = re.search(r"^ARG DEBIAN_SNAPSHOT=(\S+)$", dockerfile, re.MULTILINE)
    require(snapshot_match is not None, "Docker package snapshot is not pinned")
    snapshot = snapshot_match.group(1)
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        workflow_source = read(workflow)
        require(
            "cargo install cargo-audit --version 0.22.2 --locked" in workflow_source,
            f"{workflow} does not install the exact locked cargo-audit release",
        )
        require(
            "cargo audit --deny warnings" in workflow_source,
            f"{workflow} does not fail on RustSec warnings",
        )
        require(
            f"DEBIAN_SNAPSHOT: {snapshot}" in workflow_source,
            f"{workflow} hostile rootfs does not use Docker's Debian snapshot {snapshot}",
        )
        require(
            'test "$(uname -m)" = x86_64' in workflow_source,
            f"{workflow} hostile gate does not enforce x86_64",
        )
        require(
            'echo 1 > "$probe/cgroup.kill"' in workflow_source,
            f"{workflow} hostile gate does not write-probe cgroup.kill",
        )
        for image_mode_contract in [
            'test "$(stat -c %a /opt/coop)" = 700',
            'test "$(stat -c %U:%G /opt/coop)" = root:root',
            'test "$(stat -c %a /opt/coop/rootfs)" = 755',
            'test "$(stat -c %U:%G /opt/coop/rootfs)" = root:root',
            'test "$(stat -c %a /data)" = 700',
            'test "$(stat -c %U:%G /data)" = root:root',
            'test "$(stat -c %a /var/lib/coop/jobs)" = 700',
            'test "$(stat -c %U:%G /var/lib/coop/jobs)" = root:root',
        ]:
            require(
                image_mode_contract in workflow_source,
                f"{workflow} does not validate final image mode: {image_mode_contract}",
            )
        require(
            "cargo test --locked -p coop-exec --test namespace_umask "
            "restrictive_umask_preserves_private_device_access -- --exact --ignored --nocapture"
            in workflow_source,
            f"{workflow} does not gate private devices under a restrictive umask",
        )
        for python_version in ["3.9", "3.14"]:
            require(
                f'python-version: "{python_version}"' in workflow_source,
                f"{workflow} does not test Python {python_version}",
            )
        for invariant in [
            "build==1.3.0 twine==7.0.0",
            "python -m build --wheel --sdist --outdir",
            "python -m twine check",
            "python -m pip install --no-deps --target",
            'PYTHONPATH="$target" python -S -m unittest discover -s sdks/python/tests -v',
            "import coop, coop_mcp",
            "coop_mcp.py",
            "-name 'coop_sdk-*.whl'",
            "-name 'coop_sdk-*.tar.gz'",
        ]:
            require(
                invariant in workflow_source,
                f"{workflow} omits the Python distribution gate: {invariant}",
            )

    release = read(".github/workflows/release.yml")
    workflow_text = read(".github/workflows/ci.yml") + release
    for moving_or_retiring in ["macos-14", "ubuntu-latest", "windows-latest"]:
        require(
            moving_or_retiring not in workflow_text,
            f"moving or retiring runner label is still configured: {moving_or_retiring}",
        )
    require("os: macos-15" in release, "Apple-silicon release is not built on the GA arm64 runner")
    require("COOP_RELEASE_GOVERNANCE" in release, "release workflow lacks repository-governance acknowledgement")
    require(
        "environment:\n      name: release" in release,
        "publish job lacks the protected release environment hook",
    )
    require(
        "immutable releases" in read("docs/releasing.md"),
        "release governance docs omit published-asset immutability",
    )
    require(
        "x86_64-unknown-linux-musl" in release,
        "release workflow omits the supported Linux x86_64 artifact",
    )
    require(
        "aarch64-unknown-linux" not in release,
        "release workflow publishes an unsupported Linux architecture",
    )
    require("coop-sandbox-init" in release, "Linux release artifact omits the sandbox helper")
    python_manifest = read("sdks/python/pyproject.toml")
    for required in [
        'coop-mcp = "coop_mcp:main"',
        '"coop_mcp.py" = "coop_mcp.py"',
    ]:
        require(required in python_manifest, f"Python MCP packaging contract missing: {required}")
    root_license = read("LICENSE")
    for relative in ["sdks/python/LICENSE", "sdks/typescript/LICENSE"]:
        require(
            read(relative) == root_license,
            f"{relative} differs from the canonical repository license",
        )
    bootstrap = read("scripts/bootstrap-production.sh")
    for required in [
        "COOP_PRODUCTION_VM_ACKNOWLEDGED",
        'test "$(uname -m)" = x86_64',
        "scripts/verify-production.py",
        "target.write_text",
        "target.chmod(0o600)",
    ]:
        require(required in bootstrap, f"production bootstrap contract missing: {required}")
    container_smoke = read("scripts/smoke-container.sh")
    for required in [
        "scripts/verify-production.py",
        "COOP_VERIFY_LANGUAGES=python,node,bash",
    ]:
        require(required in container_smoke, f"container smoke contract missing: {required}")
    production_verifier = read("scripts/verify-production.py")
    for required in [
        "verify_canary",
        "verify_receipt",
        'receipt.get("receipt_sha256")',
        'document.get("networking") == "disabled"',
        'policy.get("network_allowed") is False',
        'hashlib.sha256(canonical.encode("utf-8")).hexdigest() == recorded',
    ]:
        require(required in production_verifier, f"production verifier contract missing: {required}")
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        require(
            "bash scripts/smoke-container.sh" in read(workflow),
            f"{workflow} does not smoke the final container image",
        )
        require(
            'COOP_SMOKE_ALLOW_PRIVILEGED: "true"' in read(workflow),
            f"{workflow} does not explicitly acknowledge the privileged image smoke",
        )
    require(
        "docs deploy sdks integrations" in release,
        "release archives omit docs, deploy templates, SDKs, or integrations",
    )
    require("path: sdk-dist/*" in release, "release workflow does not upload every SDK distribution")
    require("npm pack --pack-destination" in release, "release workflow omits the npm tarball")
    normalized_release = " ".join(release.split())
    checksum_pipeline = (
        'manifest="${RUNNER_TEMP}/coop-SHA256SUMS-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}" '
        "find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%f\\0' \\ "
        "| sort -z \\ "
        "| xargs -0 sha256sum -- \\ "
        '> "$manifest" mv -- "$manifest" SHA256SUMS'
    )
    require(
        checksum_pipeline in normalized_release,
        "release workflow must deterministically checksum every asset and exclude its manifest",
    )
    require("spdx-json" in release, "release workflow omits the SPDX SBOM")
    require("actions/attest@" in release, "release workflow omits provenance")
    require("draft: true" in release, "release assets must stage in a draft")
    require("--draft=false" in release, "release workflow never atomically publishes its draft")

    compose = read("docker-compose.yml")
    require("127.0.0.1:7300:7300" in compose, "Compose must publish Coop on loopback only")
    require("COOP_API_KEYS: \"${COOP_API_KEYS:?" in compose, "Compose must require an API key")
    require("privileged: true" in compose, "namespace Compose posture changed; re-review boundary docs")
    require(read(".env.example").split("COOP_API_KEYS=", 1)[1].splitlines()[0] == "", ".env example must not contain a key")
    return action_count


def check_documented_boundary() -> None:
    required_claims = {
        "README.md": ["Linux x86_64-only", "other Linux architectures"],
        "SECURITY.md": ["Linux x86_64", "non-x86_64 Linux"],
        "docs/deployment.md": ["x86_64 Linux host", "Non-x86_64 Linux"],
        "docs/security-boundary.md": ["Linux x86_64", "other Linux architectures"],
        "docs/troubleshooting.md": ["x86_64 Linux 5.14+", "non-x86_64 Linux"],
    }
    for relative, claims in required_claims.items():
        document = read(relative)
        for claim in claims:
            require(
                claim in document,
                f"{relative} omits the current architecture boundary: {claim}",
            )
    service = read("deploy/coop.service")
    require(
        "ConditionArchitecture=x86-64" in service,
        "systemd production template does not reject unsupported architectures",
    )


def check_transport_guards() -> None:
    transport = read("crates/coop-server/src/transport.rs")
    for invariant in [
        "HTTP_WRITE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30)",
        "HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30)",
        "HTTP_CONNECTION_MAX_LIFETIME: Duration = Duration::from_secs(10 * 60)",
        "HTTP_MAX_ACCEPTED_CONNECTIONS: usize = 256",
        "hyper::server::conn::http1::Builder",
        ".timer(TokioTimer::new())",
        ".header_read_timeout(header_read_timeout)",
        ".acquire_many_owned(max_connections)",
        "ForceCloseOnDrop::new",
        "dropping_server_force_closes_upgraded_socket_and_reclaims_capacity",
    ]:
        require(invariant in transport, f"HTTP transport invariant missing: {invariant}")

    caddy = read("deploy/Caddyfile.example")
    for invariant in [
        "read_body 30s",
        "read_header 30s",
        "write 6m",
        "idle 10m",
        "max_header_size 32KB",
    ]:
        require(invariant in caddy, f"Caddy transport guard missing: {invariant}")

    api = read("docs/api.md")
    for claim in [
        "at most 256 connections",
        "every HTTP/1 request head a total 30 seconds",
        "absolute 10 minutes",
    ]:
        require(claim in api, f"API docs omit transport invariant: {claim}")


def check_hostile_count() -> int:
    source = read("crates/coop-server/tests/hostile.rs")
    count = len(re.findall(r"#\[(?:tokio::)?test\]\s*#\[ignore\]", source))
    require(count > 0, "no ignored hostile containment tests were discovered")
    expected = f'test "$count" -eq {count}'
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        require(expected in read(workflow), f"{workflow} hostile discovery guard must expect {count}")
    return count


def main() -> int:
    try:
        version = check_versions()
        links = check_markdown_links()
        actions = check_pins_and_packaging()
        check_documented_boundary()
        check_transport_guards()
        hostile = check_hostile_count()
    except (AssertionError, KeyError, OSError, json.JSONDecodeError) as error:
        print(f"release-surface check failed: {error}", file=sys.stderr)
        return 1
    print(
        f"release surface OK: v{version}, {links} local links, "
        f"{actions} pinned action uses, {hostile} hostile tests"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
