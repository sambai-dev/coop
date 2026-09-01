#!/usr/bin/env python3
"""Fail when release metadata, documentation, or delivery policy drifts.

The checker intentionally uses only the Python standard library so it can run
in a fresh checkout before language dependencies are installed.
"""

from __future__ import annotations

import json
import re
import runpy
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]

CURRENT_CONSUMER_SURFACES = [
    "README.md",
    "docs/index.md",
    "docs/getting-started/quickstart.md",
    "docs/getting-started/installation.md",
    "docs/use/cli.md",
    "docs/use/mcp.md",
    "docs/sdks.md",
    "sdks/python/README.md",
    "sdks/typescript/README.md",
]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def check_release_status_language(
    release_status: str, version: str, surface_sources: dict[str, str]
) -> None:
    """Keep candidate and current consumer copy aligned with release state."""
    if release_status == "candidate":
        for relative in ["README.md", "docs/index.md"]:
            require(
                "release candidate" in surface_sources[relative].casefold(),
                f"{relative} presents unpublished v{version} as current",
            )
        return

    forbidden_phrases = [
        "release candidate",
        f"after v{version} publishes",
        "until then",
    ]
    for relative, source in surface_sources.items():
        folded = source.casefold()
        for phrase in forbidden_phrases:
            require(
                phrase not in folded,
                f"{relative} retains pre-publication wording for current v{version}: {phrase}",
            )


def check_release_data(version: str) -> dict[str, str]:
    """Validate the canonical public release URLs and consumer surfaces."""
    release_data = json.loads(read("release.json"))
    require(isinstance(release_data, dict), "release.json must contain an object")
    release_status = release_data.get("release_status")
    require(release_status in {"candidate", "current"}, "release.json has an invalid release_status")
    base = f"https://github.com/sambai-dev/rookhold/releases/download/v{version}"
    expected = {
        "current_version": version,
        "release_url": f"https://github.com/sambai-dev/rookhold/releases/tag/v{version}",
        "windows_app_url": f"{base}/rookhold-x86_64-pc-windows-msvc.zip",
        "mac_app_url": f"{base}/rookhold-aarch64-apple-darwin.tar.gz",
        "linux_app_url": f"{base}/rookhold-x86_64-unknown-linux-musl.tar.gz",
        "windows_client_url": f"{base}/rookhold-cli-x86_64-pc-windows-msvc.exe",
        "mac_client_url": f"{base}/rookhold-cli-aarch64-apple-darwin",
        "linux_client_url": f"{base}/rookhold-cli-x86_64-unknown-linux-gnu",
        "python_wheel_url": f"{base}/rookhold-{version}-py3-none-any.whl",
        "python_sdist_url": f"{base}/rookhold-{version}.tar.gz",
        "npm_tarball_url": f"{base}/rookhold-{version}.tgz",
        "spdx_url": f"{base}/rookhold-{version}.spdx.json",
        "checksums_url": f"{base}/SHA256SUMS",
    }
    actual_contract = {key: value for key, value in release_data.items() if key != "release_status"}
    require(actual_contract == expected, "release.json differs from the eleven-asset release contract")

    consumer_surfaces = {
        "README.md": [
            expected["release_url"],
            expected["windows_app_url"],
            expected["mac_app_url"],
            expected["linux_app_url"],
            expected["windows_client_url"],
            expected["mac_client_url"],
            expected["linux_client_url"],
            "pip install rookhold",
            "npm install rookhold",
        ],
        "docs/index.md": [
            expected["windows_app_url"],
            expected["mac_app_url"],
            expected["linux_app_url"],
            expected["windows_client_url"],
            expected["mac_client_url"],
            expected["linux_client_url"],
            "pip install rookhold",
            "npm install rookhold",
        ],
        "docs/getting-started/quickstart.md": [
            expected["windows_app_url"],
            expected["mac_app_url"],
            expected["linux_app_url"],
        ],
        "docs/getting-started/installation.md": [
            expected[key]
            for key in [
                "release_url",
                "windows_app_url",
                "mac_app_url",
                "linux_app_url",
                "windows_client_url",
                "mac_client_url",
                "linux_client_url",
            ]
        ],
        "docs/sdks.md": [
            expected["python_wheel_url"],
            expected["python_sdist_url"],
            expected["npm_tarball_url"],
            expected["spdx_url"],
            expected["checksums_url"],
        ],
    }
    for relative, required_values in consumer_surfaces.items():
        source = read(relative)
        for required_value in required_values:
            require(required_value in source, f"{relative} differs from release.json: {required_value}")

    current_sources = {relative: read(relative) for relative in CURRENT_CONSUMER_SURFACES}
    check_release_status_language(release_status, version, current_sources)

    for relative, source in current_sources.items():
        advertised_versions = set(re.findall(r"\bv(\d+\.\d+\.\d+)\b", source))
        require(
            advertised_versions <= {version},
            f"{relative} advertises stale versions: {sorted(advertised_versions - {version})}",
        )
    return release_data


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
    require(version == "0.8.0", f"unexpected workspace release version: {version}")
    check_release_data(version)

    toolchain = toml_string("rust-toolchain.toml", "toolchain", "channel")
    rust_version = toml_string("Cargo.toml", "workspace.package", "rust-version")
    require(toolchain == f"{rust_version}.0", "Cargo and rust-toolchain Rust versions differ")
    root_manifest = read("Cargo.toml")
    lockfile = read("Cargo.lock")
    for crate in ["coop-attestation", "coop-types", "coop-store", "coop-exec", "coop-server"]:
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
    for dependency in ["coop-attestation", "coop-types", "coop-store", "coop-exec"]:
        require(
            re.search(
                rf'^{re.escape(dependency)}\s*=\s*\{{[^\n}}]*version\s*=\s*"{re.escape(version)}"',
                root_manifest,
                re.MULTILINE,
            )
            is not None,
            f"workspace dependency {dependency} does not require {version}",
        )
    require(
        re.search(
            r'^jsonwebtoken\s*=\s*\{[^\n}]*version\s*=\s*"10\.4\.0"[^\n}]*features\s*=\s*\["aws_lc_rs"\]',
            root_manifest,
            re.MULTILINE,
        )
        is not None,
        "JWT verification must use patched jsonwebtoken 10.4.0 with the explicit AWS-LC backend",
    )
    require(
        re.search(r'\[\[package\]\]\s+name = "jsonwebtoken"\s+version = "10\.4\.0"', lockfile)
        is not None,
        "Cargo.lock does not contain patched jsonwebtoken 10.4.0",
    )
    server_manifest = read("crates/coop-server/Cargo.toml")
    require(
        re.search(
            r'^aws-lc-rs\s*=\s*\{[^\n}]*version\s*=\s*"1\.18\.0"[^\n}]*default-features\s*=\s*false',
            root_manifest,
            re.MULTILINE,
        )
        is not None,
        "workspace must pin the reviewed aws-lc-rs 1.18.0 dependency",
    )
    require(
        re.search(
            r'^aws-lc-rs\s*=\s*\{[^\n}]*workspace\s*=\s*true[^\n}]*features\s*=\s*\["prebuilt-nasm"\]',
            server_manifest,
            re.MULTILINE,
        )
        is not None,
        "Windows x86_64 builds must enable the checked-in AWS-LC NASM objects",
    )
    require(
        re.search(r'\[\[package\]\]\s+name = "aws-lc-rs"\s+version = "1\.18\.0"', lockfile)
        is not None,
        "Cargo.lock does not contain reviewed aws-lc-rs 1.18.0",
    )
    cargo_config = read(".cargo/config.toml")
    require(
        re.search(
            r'^AWS_LC_SYS_USE_SYSTEM\s*=\s*\{\s*value\s*=\s*"0"\s*,\s*force\s*=\s*true\s*\}$',
            cargo_config,
            re.MULTILINE,
        )
        is not None,
        "Cargo builds must use the lockfile-pinned bundled AWS-LC source",
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
        f'__version__ = "{version}"' in read("sdks/python/rookhold.py"),
        "Python module version differs from its package metadata",
    )
    require(
        f'__version__ = "{version}"' in read("sdks/python/rookhold_mcp.py"),
        "Python MCP module version differs from its package metadata",
    )
    require(
        "from rookhold import IsolationClass, Rookhold, RookholdError, __version__"
        in read("sdks/python/rookhold_cli.py"),
        "Python CLI does not inherit the SDK release version",
    )
    typescript_source = read("sdks/typescript/rookhold.ts")
    typescript_client_header = f'"X-Rookhold-Client": "typescript/{version}"'
    require(
        typescript_source.count(typescript_client_header) == 2,
        "TypeScript HTTP and artifact clients do not identify the package release version",
    )
    require(
        'const moduleName = "node:crypto"' in typescript_source,
        "TypeScript SDK omits the Node 18 Web Crypto fallback",
    )
    python_lock = read("sdks/python/uv.lock")
    locked_python_project = re.search(
        r'\[\[package\]\]\s+name = "rookhold"\s+version = "([^"]+)"',
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
        workflow_source = read(workflow)
        require(
            f"assert rookhold.__version__ == '{version}'" in workflow_source,
            f"{workflow} Python distribution assertion differs from the workspace version",
        )
        for configured in re.findall(r"^\s*toolchain:\s*([^\s#]+)", workflow_source, re.MULTILINE):
            require(
                configured == toolchain,
                f"{workflow} installs Rust {configured}, expected {toolchain}",
            )
    require(
        f'VERSION: "{version}"' in read("docker-compose.yml"),
        "Compose build version differs from the workspace",
    )
    changelog = read("CHANGELOG.md")
    release_headings = re.findall(r"^## (.+)$", changelog, re.MULTILINE)
    require(bool(release_headings), "CHANGELOG contains no release headings")
    require(
        re.fullmatch(rf"{re.escape(version)} — \d{{4}}-\d{{2}}-\d{{2}}", release_headings[0])
        is not None,
        "CHANGELOG first release heading is not the finalized workspace version",
    )
    first_release = changelog.split(f"## {release_headings[0]}", 1)[1]
    first_release = first_release.split("\n## ", 1)[0]
    require("\n### " in first_release, "CHANGELOG current release has no sections")
    require("\n- " in first_release, "CHANGELOG current release has no entries")
    release_workflow = read(".github/workflows/release.yml")
    require(
        'grep -Eqi "^## ${version}.*unreleased" CHANGELOG.md' in release_workflow,
        "release workflow does not reject an unfinalized changelog",
    )
    require(
        "[v${version}](https://github.com/sambai-dev/rookhold/releases/tag/v${version})"
        in release_workflow,
        "release workflow does not require the matching README release link",
    )
    require(
        'series="${version%.*}.x"' in release_workflow
        and "| tagged \\`${series}\\` releases | Supported |" in release_workflow,
        "release workflow does not require the matching supported-version line",
    )
    require(
        f"[v{version}](https://github.com/sambai-dev/rookhold/releases/tag/v{version})"
        in read("README.md"),
        "README current-release link differs from the workspace version",
    )
    series = version.rsplit(".", 1)[0] + ".x"
    require(
        f"| tagged `{series}` releases | Supported |" in read("SECURITY.md"),
        "SECURITY supported-version line differs from the workspace version",
    )
    version_parts = version.split(".")
    if int(version_parts[1]) > 0:
        previous_series = f"{version_parts[0]}.{int(version_parts[1]) - 1}.x"
        require(
            f"| tagged `{previous_series}` releases | Unsupported; upgrade to {series} |"
            in read("SECURITY.md"),
            "SECURITY previous release line does not point to the supported series",
        )
    versioned_document_contracts = {
        "docs/deployment.md": [
            f"v{version} or newer",
            f"version={version}",
            f'$version = "{version}"',
        ],
        "docs/sdks.md": [
            f"v{version} release",
            f"version={version}",
            f"rookhold-{version}.tar.gz",
        ],
        "sdks/python/README.md": [
            f"v{version}",
            f"rookhold-{version}-py3-none-any.whl",
        ],
        "sdks/typescript/README.md": [
            f"v{version}",
            f"rookhold-{version}.tgz",
        ],
        ".github/ISSUE_TEMPLATE/bug_report.yml": [f"v{version} or a commit SHA"],
    }
    for relative, expected_values in versioned_document_contracts.items():
        document = read(relative)
        for expected in expected_values:
            require(
                expected in document,
                f"{relative} omits current release value: {expected}",
            )
    return version


def check_markdown_links() -> int:
    markdown_files = [
        *(ROOT / name for name in ["README.md", "SECURITY.md", "AUDIT.md", "CHANGELOG.md"]),
        *(ROOT / "docs").glob("*.md"),
        *(ROOT / "integrations").glob("**/*.md"),
        ROOT / "sdks" / "python" / "README.md",
        ROOT / "sdks" / "typescript" / "README.md",
        ROOT / "crates" / "coop-attestation" / "README.md",
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
    version = toml_string("Cargo.toml", "workspace.package", "version")
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
        'ROOKHOLD_GIT_REVISION="${VCS_REF}" cargo build --locked --release',
        "-p coop-server -p coop-exec -p coop-attestation --bins",
        'ROOKHOLD_GIT_REVISION="${VCS_REF}" cargo build',
        "COPY .cargo ./.cargo",
        "rookhold-sandbox-init /usr/local/bin/rookhold-sandbox-init",
        "rookhold-oci-init /usr/local/bin/rookhold-oci-init",
        "rookhold-verify /usr/local/bin/rookhold-verify",
        "scripts/container-entrypoint.sh /usr/local/bin/rookhold-container-entrypoint",
        'ENTRYPOINT ["/usr/local/bin/rookhold-container-entrypoint"]',
        'CMD ["/usr/local/bin/rookhold"]',
        "ROOKHOLD_ROOTFS=/opt/rookhold/rootfs",
        "ROOKHOLD_SANDBOX_HELPER=/usr/local/bin/rookhold-sandbox-init",
        "install -d -o root -g root -m 0700 /data /var/lib/rookhold/jobs /opt/rookhold",
        "install -d -o root -g root -m 0755 /opt/rookhold/rootfs",
        "/run/rookhold-bootstrap /run/rookhold-runtime /run/rookhold-secrets",
        'test "$(dpkg --print-architecture)" = amd64',
    ]:
        require(required in dockerfile, f"Docker helper/rootfs contract missing: {required}")

    dockerignore_lines = {
        line.strip()
        for line in read(".dockerignore").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }
    for required in ["target", "target-*", ".coop-runtime"]:
        require(required in dockerignore_lines, f"Docker context does not exclude {required}")

    snapshot_match = re.search(r"^ARG DEBIAN_SNAPSHOT=(\S+)$", dockerfile, re.MULTILINE)
    require(snapshot_match is not None, "Docker package snapshot is not pinned")
    snapshot = snapshot_match.group(1)
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        workflow_source = read(workflow)
        require(
            "ROOKHOLD_GIT_REVISION: ${{ github.sha }}" in workflow_source,
            f"{workflow} does not embed the checked-out revision",
        )
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
            'test "$(stat -c %a /opt/rookhold)" = 700',
            'test "$(stat -c %U:%G /opt/rookhold)" = root:root',
            'test "$(stat -c %a /opt/rookhold/rootfs)" = 755',
            'test "$(stat -c %U:%G /opt/rookhold/rootfs)" = root:root',
            'test "$(stat -c %a /data)" = 700',
            'test "$(stat -c %U:%G /data)" = root:root',
            'test "$(stat -c %a /var/lib/rookhold/jobs)" = 700',
            'test "$(stat -c %U:%G /var/lib/rookhold/jobs)" = root:root',
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
            "import coop, coop_mcp, rookhold, rookhold_cli, rookhold_mcp",
            "coop_mcp.py",
            "-name 'rookhold-*.whl'",
            "-name 'rookhold-*.tar.gz'",
        ]:
            require(
                invariant in workflow_source,
                f"{workflow} omits the Python distribution gate: {invariant}",
            )

    release = read(".github/workflows/release.yml")
    for revision_contract in [
        '[[ "$ROOKHOLD_GIT_REVISION" =~ ^[0-9a-f]{40}$ ]]',
        "checkout_revision=$(git rev-parse --verify 'HEAD^{commit}')",
        'test "$ROOKHOLD_GIT_REVISION" = "$checkout_revision"',
    ]:
        require(
            revision_contract in release,
            f"release preflight omits revision contract: {revision_contract}",
        )
    workflow_text = read(".github/workflows/ci.yml") + release
    for moving_or_retiring in ["macos-14", "ubuntu-latest", "windows-latest"]:
        require(
            moving_or_retiring not in workflow_text,
            f"moving or retiring runner label is still configured: {moving_or_retiring}",
        )
    require("os: macos-15" in release, "Apple-silicon release is not built on the GA arm64 runner")
    require("ROOKHOLD_RELEASE_GOVERNANCE" in release, "release workflow lacks repository-governance acknowledgement")
    require("ROOKHOLD_PYPI_TRUSTED_PUBLISHER" in release, "release workflow lacks PyPI publisher acknowledgement")
    require("ROOKHOLD_NPM_TRUSTED_PUBLISHER" in release, "release workflow lacks npm publisher acknowledgement")
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
    require("rookhold-sandbox-init" in release, "Linux release artifact omits the sandbox helper")
    require("rookhold-oci-init" in release, "Linux release artifact omits the gVisor OCI init")
    require("rookhold-verify" in release, "release artifacts omit the offline attestation verifier")
    gvisor_smoke = read("scripts/smoke-gvisor.sh")
    for required in [
        "release-20260817.0",
        "048b89aada69dc3333422e139d6e9d02f8ab06bda52398060e0fbdacca00074c",
        "gvisor-application-kernel",
        "NETWORK_BLOCKED",
        "crash reconciliation passed",
        "ROOKHOLD_ATTESTATION_MODE=sign",
        "rookhold-verify",
    ]:
        require(required in gvisor_smoke, f"gVisor release gate missing contract: {required}")
    python_manifest = read("sdks/python/pyproject.toml")
    for required in [
        'requires = ["hatchling==1.27.0"]',
        'rookhold-cli = "rookhold_cli:main"',
        'rookhold-mcp = "rookhold_mcp:main"',
        'coop-mcp = "coop_mcp:main"',
        '"rookhold.py" = "rookhold/__init__.py"',
        '"rookhold_cli.py" = "rookhold_cli.py"',
        '"coop_mcp.py" = "coop_mcp.py"',
    ]:
        require(required in python_manifest, f"Python MCP packaging contract missing: {required}")
    claude_mcp = json.loads(read("integrations/claude-code/mcp.json"))
    claude_rookhold = claude_mcp.get("mcpServers", {}).get("rookhold", {})
    require(
        claude_rookhold.get("type") == "stdio"
        and claude_rookhold.get("command") == "rookhold-cli"
        and claude_rookhold.get("args") == ["mcp-server"]
        and "${ROOKHOLD_API_KEY}" in claude_rookhold.get("env", {}).values(),
        "Claude Code template must launch the single-file CLI MCP mode with an environment reference",
    )
    opencode_mcp = json.loads(read("integrations/opencode/opencode.snippet.json"))
    opencode_rookhold = (
        opencode_mcp.get("mcp", {}).get("servers", {}).get("rookhold", {})
    )
    require(
        opencode_rookhold.get("type") == "local"
        and opencode_rookhold.get("command") == ["rookhold-cli", "mcp-server"]
        and "{env:ROOKHOLD_API_KEY}"
        in opencode_rookhold.get("environment", {}).values(),
        "OpenCode v2 template must launch the single-file CLI MCP mode with an environment reference",
    )
    root_license = read("LICENSE")
    for relative in ["sdks/python/LICENSE", "sdks/typescript/LICENSE"]:
        require(
            read(relative) == root_license,
            f"{relative} differs from the canonical repository license",
        )
    bootstrap = read("scripts/bootstrap-production.sh")
    for required in [
        "ROOKHOLD_PRODUCTION_VM_ACKNOWLEDGED",
        'test "$(uname -m)" = x86_64',
        "scripts/verify-production.py",
        "ROOKHOLD_VERIFY_MINIMUM_ISOLATION",
        "reviewed_runsc_sha256",
        "ROOKHOLD_GVISOR_ROOTFS_SHA256",
        "attestation-key.pem",
        "attestation-public-key.pem",
        "ROOKHOLD_VERIFY_CONTAINER_IMAGE",
        "/usr/local/bin/rookhold-verify public-key",
        "gvisor-application-kernel",
        "target.write_text",
        "target.chmod(0o600)",
    ]:
        require(required in bootstrap, f"production bootstrap contract missing: {required}")
    container_smoke = read("scripts/smoke-container.sh")
    for required in [
        "scripts/verify-production.py",
        "scripts/verify-python-adapter.py",
        "ROOKHOLD_VERIFY_LANGUAGES=python,node,bash",
        "ROOKHOLD_VERIFY_MINIMUM_ISOLATION=linux-shared-kernel",
        "ROOKHOLD_ATTESTATION_MODE=sign",
        "ROOKHOLD_ATTESTATION_KEY_FILE",
        "ROOKHOLD_ATTESTATION_KEY_SOURCE",
        "ROOKHOLD_VERIFY_PUBLIC_KEY_FILE",
        "ROOKHOLD_VERIFY_CONTAINER_IMAGE",
        'host_uid=$(id -u)',
        '--user "$host_uid:$host_gid"',
        "/usr/local/bin/rookhold-verify generate-key",
        "/usr/local/bin/rookhold-verify public-key",
    ]:
        require(required in container_smoke, f"container smoke contract missing: {required}")
    require(
        "openssl genpkey" not in container_smoke,
        "container smoke must generate canonical keys with packaged rookhold-verify",
    )
    production_verifier = read("scripts/verify-production.py")
    for required in [
        "verify_canary",
        "verify_receipt",
        "parse_minimum_isolation",
        "isolation_satisfies",
        "ROOKHOLD_VERIFY_MINIMUM_ISOLATION",
        '"requirements": {"minimum_isolation": minimum_isolation}',
        'for field in ["runtime_sha256", "rootfs_sha256", "config_sha256"]',
        'receipt.get("receipt_sha256")',
        'api.request("GET", "/v1/whoami")',
        'predicate.get("tenant") == tenant',
        'artifact_document.get("tenant") == tenant',
        '"--tenant",',
        'output.get("tenant") == tenant',
        'document.get("networking") == "disabled"',
        'hashlib.sha256(canonical.encode("utf-8")).hexdigest() == recorded',
        "OfflineVerifier",
        "ROOKHOLD_VERIFY_PUBLIC_KEY_FILE",
        "ROOKHOLD_VERIFY_BIN",
        '"--subject-name"',
        '"--public-key"',
        "/usr/local/bin/rookhold-verify",
    ]:
        require(required in production_verifier, f"production verifier contract missing: {required}")
    require(
        'api.request("GET", "/v1/attestation/public-key")' not in production_verifier,
        "production verifier must not trust the server public-key endpoint",
    )
    container_entrypoint = read("scripts/container-entrypoint.sh")
    for required in [
        "ROOKHOLD_GVISOR_RUNSC_SOURCE",
        "ROOKHOLD_ATTESTATION_KEY_SOURCE",
        "ROOKHOLD_VERIFY_PUBLIC_KEY_SOURCE",
        "install -o 0 -g 0",
    ]:
        require(required in container_entrypoint, f"container staging contract missing: {required}")
    require(
        "python3 -m unittest discover -s scripts/tests -v"
        in read(".github/workflows/ci.yml"),
        "CI does not run production verifier provider fixtures",
    )
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        require(
            "bash scripts/smoke-container.sh" in read(workflow),
            f"{workflow} does not smoke the final container image",
        )
        require(
            'ROOKHOLD_SMOKE_ALLOW_PRIVILEGED: "true"' in read(workflow),
            f"{workflow} does not explicitly acknowledge the privileged image smoke",
        )
    require(
        "docs deploy sdks integrations" in release,
        "release archives omit docs, deploy templates, SDKs, or integrations",
    )
    for standalone_contract in [
        "pyinstaller==6.22.2",
        "pyinstaller-hooks-contrib==2026.7",
        "--name rookhold-cli",
        "--name rookhold-mcp",
        "Smoke standalone terminal apps without Python packaging",
        "Stage the direct single-file CLI download",
        "mcp-server",
        "standalone-dist/rookhold-cli",
        "standalone-dist/rookhold-mcp",
    ]:
        require(
            standalone_contract in release,
            f"standalone terminal release contract missing: {standalone_contract}",
        )
    require("path: sdk-dist/*" in release, "release workflow does not upload every SDK distribution")
    require("npm pack --pack-destination" in release, "release workflow omits the npm tarball")
    release_helper_source = read("scripts/release-artifacts.py")
    release_helper = runpy.run_path(str(ROOT / "scripts" / "release-artifacts.py"))
    payload_names = release_helper["payload_names"](version)
    release_names = release_helper["release_names"](version)
    require(len(payload_names) == 9, "release payload contract must contain exactly nine files")
    require(len(release_names) == 11, "public release contract must contain exactly eleven assets")
    for asset in release_names:
        workflow_asset = asset.replace(version, "${{ needs.preflight.outputs.version }}")
        require(
            f"dist/{workflow_asset}" in release,
            f"release workflow does not explicitly stage {asset}",
        )
    for artifact_name in [
        "rookhold-sdks",
        "rookhold-x86_64-unknown-linux-musl",
        "rookhold-aarch64-apple-darwin",
        "rookhold-x86_64-pc-windows-msvc",
    ]:
        require(
            f"name: {artifact_name}" in release,
            f"publish job does not download exact workflow artifact {artifact_name}",
        )
    for forbidden in ["merge-multiple: true", "files: dist/*", "subject-path: dist/*"]:
        require(forbidden not in release, f"release workflow still uses an unbounded wildcard: {forbidden}")
    for helper_command in ["assemble", "prepare", "verify-sbom", "finalize", "verify-release"]:
        require(
            f"scripts/release-artifacts.py {helper_command}" in release,
            f"release workflow omits artifact reconciliation phase: {helper_command}",
        )
    for sbom_contract in [
        "path: sbom-input",
        "syft-version: v1.51.1",
        "SYFT_FILE_METADATA_SELECTION: all",
        "SYFT_SOURCE_NAME: rookhold-release-${{ needs.preflight.outputs.version }}",
        "sbom-path: dist/rookhold-${{ needs.preflight.outputs.version }}.spdx.json",
        "--predicate-type https://spdx.dev/Document/v2.3",
    ]:
        require(sbom_contract in release, f"artifact-scoped SBOM contract missing: {sbom_contract}")
    for provenance_contract in [
        "--signer-workflow",
        "--source-ref",
        "--source-digest",
        "--signer-digest",
        "--deny-self-hosted-runners",
        "--predicate-type https://slsa.dev/provenance/v1",
    ]:
        require(
            provenance_contract in release,
            f"release attestation verification constraint missing: {provenance_contract}",
        )
    for draft_contract in [
        "overwrite_files: false",
        "--json assets,isDraft,tagName",
        "remote asset digest differs from local file",
    ]:
        require(
            draft_contract in release + release_helper_source,
            f"remote draft reconciliation contract missing: {draft_contract}",
        )
    require(
        "subject-checksums: ${{ runner.temp }}/rookhold-payload-subjects.sha256" in release
        and "subject-checksums: ${{ runner.temp }}/rookhold-release-subjects.sha256" in release,
        "release workflow must use exact checksum subject lists for SBOM and provenance",
    )
    require(release.count("actions/attest@") == 2, "release workflow must create SBOM and provenance attestations")
    require("draft: true" in release, "release assets must stage in a draft")
    stage_start = release.find("\n  stage-release:")
    registries_start = release.find("\n  publish-registries:")
    smoke_start = release.find("\n  registry-smoke:")
    publish_start = release.find("\n  publish-release:")
    require(
        min(stage_start, registries_start, smoke_start, publish_start) >= 0,
        "release workflow omits one or more ordered publication jobs",
    )
    require(
        stage_start < registries_start < smoke_start < publish_start,
        "release jobs must stage, publish registries, smoke registries, then publish GitHub",
    )
    stage_job = release[stage_start:registries_start]
    registries_job = release[registries_start:smoke_start]
    smoke_job = release[smoke_start:publish_start]
    publish_job = release[publish_start:]
    require(
        "needs: [preflight, stage-release]" in registries_job,
        "registry publication must depend on the reconciled draft",
    )
    require(
        "needs: [preflight, publish-registries]" in smoke_job,
        "registry smoke must depend on public package publication",
    )
    require(
        "needs: [preflight, stage-release, registry-smoke]" in publish_job,
        "public GitHub release must depend on both the draft and registry smoke",
    )
    require(
        "name: rookhold-release-staged" in stage_job
        and "name: rookhold-release-staged" in publish_job,
        "final publication must re-use the exact reconciled eleven-asset set",
    )
    require(
        "scripts/release-artifacts.py verify-release" in publish_job,
        "final publication must re-verify the unchanged remote draft",
    )
    require("--draft=false" not in stage_job, "draft staging must not publish the GitHub release")
    require(
        "--draft=false" in publish_job,
        "release workflow must publish its draft only after registry smoke",
    )

    install_docs = read("docs/deployment.md") + read("docs/sdks.md")
    require(
        "sha256sum --check --ignore-missing" not in install_docs,
        "installation docs still allow an unbound checksum row",
    )
    for verification_contract in [
        "set -euo pipefail",
        "gh release verify-asset",
        "--signer-workflow sambai-dev/rookhold/.github/workflows/release.yml",
        '--source-ref "refs/tags/v${version}"',
        "--predicate-type https://slsa.dev/provenance/v1",
        "--deny-self-hosted-runners",
    ]:
        require(
            verification_contract in install_docs,
            f"installation docs omit fail-closed verification: {verification_contract}",
        )
    require(
        install_docs.count("$LASTEXITCODE -ne 0") >= 5,
        "PowerShell installation path does not stop after every native verification failure",
    )

    compose = read("docker-compose.yml")
    require("127.0.0.1:7300:7300" in compose, "Compose must publish Rookhold on loopback only")
    require(
        'ROOKHOLD_API_KEYS: "${ROOKHOLD_API_KEYS:-${COOP_API_KEYS:-}}"' in compose,
        "Compose must pass through the primary API key with a legacy fallback",
    )
    for contract in [
        'ROOKHOLD_SANDBOX: "${ROOKHOLD_SANDBOX:-${COOP_SANDBOX:-gvisor}}"',
        "ROOKHOLD_GVISOR_RUNSC_SOURCE: /run/rookhold-bootstrap/runsc",
        "ROOKHOLD_GVISOR_RUNSC: /run/rookhold-runtime/runsc",
        "ROOKHOLD_GVISOR_ROOTFS_SHA256",
        "ROOKHOLD_ATTESTATION_MODE: sign",
        "ROOKHOLD_ATTESTATION_KEY_SOURCE",
        "ROOKHOLD_ATTESTATION_KEY_FILE: /run/rookhold-secrets/attestation-key.pem",
        "coop_attestation_key",
        "/run/rookhold-runtime:rw,exec,nosuid,nodev,mode=0700,uid=0,gid=0",
        "/run/rookhold-secrets:rw,noexec,nosuid,nodev,mode=0700,uid=0,gid=0",
        "privileged: true",
    ]:
        require(contract in compose, f"Compose production contract missing: {contract}")
    require(read(".env.example").split("ROOKHOLD_API_KEYS=", 1)[1].splitlines()[0] == "", ".env example must not contain a key")
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
    for relative in ["README.md", "docs/getting-started/quickstart.md"]:
        source = read(relative)
        for claim in ["network      host", "isolation    none"]:
            require(claim in source, f"{relative} omits truthful local-demo output: {claim}")
    compatibility = read(".github/workflows/compatibility.yml")
    require(
        "scripts/verify-local-demo.py local-demo.json" in compatibility,
        "compatibility workflow does not execute the local documentation snapshot",
    )
    verifier = read("scripts/verify-local-demo.py")
    for invariant in [
        'receipt.get("networking") == "host"',
        'receipt.get("isolation_class") == "none"',
    ]:
        require(invariant in verifier, f"local documentation verifier omits: {invariant}")
    require(
        '.get(format!("{base_url}/readyz"))' in read("crates/coop-server/src/cli.rs"),
        "temporary app startup does not wait for durable readiness",
    )
    service = read("deploy/rookhold.service")
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


def check_contribution_contract() -> None:
    contributing = read("CONTRIBUTING.md")
    for claim in [
        "Tier A — docs, examples, and integrations",
        "Tier B — SDK, CLI, and public API",
        "Tier C — executor, authentication, storage, receipts, and isolation",
        "Tier C definition of technically done",
        "The regression proves RED",
        "Adversarial cases are considered",
        "The final exact diff is rescanned",
        "Implementation incomplete",
        "Validation incomplete",
        "Review incomplete",
        "Integration incomplete",
        "Scoped complete",
        "Stale/unverified",
    ]:
        require(claim in contributing, f"contribution lifecycle guidance missing: {claim}")

    template = read(".github/PULL_REQUEST_TEMPLATE.md")
    for heading in [
        "## Contribution tier",
        "## Declared scope",
        "## Root invariant",
        "## Reproduction and RED evidence",
        "## Adversarial coverage",
        "## Validation",
        "## Non-goals and remaining work",
        "## Completion state",
    ]:
        require(heading in template, f"pull request template missing: {heading}")

    ci = read(".github/workflows/ci.yml")
    for contract in [
        "Verify the exact workflow revision",
        "git rev-parse --verify 'HEAD^{commit}'",
        "python3 scripts/check-pr-description.py --event",
        "scripts/check-pr-description.py",
    ]:
        require(contract in ci, f"CI completion-evidence contract missing: {contract}")


def check_hostile_count() -> int:
    source = read("crates/coop-server/tests/hostile.rs")
    count = len(re.findall(r"#\[(?:tokio::)?test\]\s*#\[ignore\]", source))
    require(count > 0, "no ignored hostile containment tests were discovered")
    expected = f'test "$count" -eq {count}'
    for workflow in [".github/workflows/ci.yml", ".github/workflows/release.yml"]:
        require(expected in read(workflow), f"{workflow} hostile discovery guard must expect {count}")
    return count


def check_plain_user_language() -> None:
    forbidden = "doc" + "tor"
    surfaces = [
        ROOT / "README.md",
        ROOT / "PRODUCT.md",
        ROOT / "CONTRIBUTING.md",
        ROOT / "sdks" / "python" / "rookhold_cli.py",
        *(ROOT / "docs").glob("**/*.md"),
        *(ROOT / "integrations").glob("**/*"),
        *(ROOT / "examples").glob("**/*"),
        *(ROOT / "templates").glob("**/*"),
    ]
    for path in surfaces:
        if (
            path.is_file()
            and path.suffix.casefold() in {".md", ".py", ".ts", ".tsx", ".json", ".yaml", ".yml"}
            and forbidden in path.read_text(encoding="utf-8").casefold()
        ):
            raise AssertionError(f"user-facing language includes a forbidden term: {path}")


def main() -> int:
    try:
        version = check_versions()
        links = check_markdown_links()
        actions = check_pins_and_packaging()
        check_documented_boundary()
        check_transport_guards()
        check_contribution_contract()
        check_plain_user_language()
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
