from __future__ import annotations

import io
import json
import runpy
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
RELEASE = runpy.run_path(str(ROOT / "scripts" / "release-artifacts.py"))
VERSION = "0.7.1"


def archive_entries(asset: str) -> dict[str, bytes | None]:
    entries: dict[str, bytes | None] = {}
    for raw_path in RELEASE["required_paths"](asset, VERSION):
        path = PurePosixPath(raw_path)
        if path.name in {"docs", "deploy", "sdks", "integrations", "tests"}:
            entries[path.as_posix()] = None
        else:
            entries[path.as_posix()] = ("fixture:" + path.as_posix()).encode()
    return entries


def write_tar(path: Path, entries: dict[str, bytes | None]) -> None:
    with tarfile.open(path, mode="w:gz") as archive:
        for name, content in entries.items():
            member = tarfile.TarInfo(name + ("/" if content is None else ""))
            if content is None:
                member.type = tarfile.DIRTYPE
                member.mode = 0o755
                archive.addfile(member)
            else:
                member.size = len(content)
                member.mode = (
                    0o755
                    if Path(name).name.startswith(("rookhold", "coop"))
                    else 0o644
                )
                archive.addfile(member, io.BytesIO(content))


def write_zip(path: Path, entries: dict[str, bytes | None]) -> None:
    with zipfile.ZipFile(path, mode="w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, content in entries.items():
            archive.writestr(
                name + ("/" if content is None else ""),
                b"" if content is None else content,
            )


def write_payload(path: Path) -> None:
    entries = archive_entries(path.name)
    if path.name.endswith((".tar.gz", ".tgz")):
        write_tar(path, entries)
    else:
        write_zip(path, entries)


class ReleaseArtifactsTests(unittest.TestCase):
    def test_exact_contract_prepare_finalize_and_remote_reconciliation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            downloads = root / "downloads"
            containers = {
                "rookhold-sdks": (
                    f"rookhold-sdk-{VERSION}.tgz",
                    f"rookhold_sdk-{VERSION}-py3-none-any.whl",
                    f"rookhold_sdk-{VERSION}.tar.gz",
                ),
                "rookhold-aarch64-apple-darwin": ("rookhold-aarch64-apple-darwin.tar.gz",),
                "rookhold-x86_64-pc-windows-msvc": ("rookhold-x86_64-pc-windows-msvc.zip",),
                "rookhold-x86_64-unknown-linux-musl": (
                    "rookhold-x86_64-unknown-linux-musl.tar.gz",
                ),
            }
            for container, assets in containers.items():
                directory = downloads / container
                directory.mkdir(parents=True)
                for asset in assets:
                    write_payload(directory / asset)

            dist = root / "dist"
            RELEASE["assemble"](downloads, dist, VERSION)
            sbom_input = root / "sbom-input"
            payload_checksums = root / "payloads.sha256"
            RELEASE["prepare"](dist, sbom_input, payload_checksums, VERSION)
            self.assertEqual(len(payload_checksums.read_text().splitlines()), 6)

            sbom = dist / RELEASE["sbom_name"](VERSION)
            document = {
                "spdxVersion": "SPDX-2.3",
                "name": f"rookhold-release-{VERSION}",
                "packages": [
                    {"name": f"rookhold-release-{VERSION}", "versionInfo": VERSION},
                    {"name": "fixture-package", "versionInfo": "1"},
                ],
                "files": [
                    {"fileName": f"./{asset}/.rookhold-release-artifact.json"}
                    for asset in RELEASE["payload_names"](VERSION)
                ],
            }
            sbom.write_text(json.dumps(document), encoding="utf-8")
            RELEASE["verify_sbom"](sbom, VERSION)

            all_checksums = root / "all.sha256"
            RELEASE["finalize"](dist, all_checksums, VERSION)
            self.assertEqual(len((dist / "SHA256SUMS").read_text().splitlines()), 7)
            self.assertEqual(len(all_checksums.read_text().splitlines()), 8)

            metadata = root / "release.json"
            metadata.write_text(
                json.dumps(
                    {
                        "tagName": f"v{VERSION}",
                        "isDraft": True,
                        "assets": [
                            {
                                "name": name,
                                "state": "uploaded",
                                "size": (dist / name).stat().st_size,
                                "digest": "sha256:" + RELEASE["sha256"](dist / name),
                            }
                            for name in RELEASE["release_names"](VERSION)
                        ],
                    }
                ),
                encoding="utf-8",
            )
            RELEASE["verify_release"](dist, metadata, VERSION, f"v{VERSION}")

    def test_unsafe_tar_member_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive_path = root / "unsafe.tar.gz"
            write_tar(archive_path, {"../escape": b"bad"})
            with self.assertRaisesRegex(ValueError, "unsafe path"):
                RELEASE["extract_tar"](archive_path, root / "output")
            self.assertFalse((root / "escape").exists())

    def test_extra_release_asset_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for name in RELEASE["payload_names"](VERSION):
                (directory / name).write_bytes(b"fixture")
            (directory / "unexpected.bin").write_bytes(b"unexpected")
            with self.assertRaisesRegex(ValueError, "unexpected"):
                RELEASE["require_exact_files"](
                    directory,
                    RELEASE["payload_names"](VERSION),
                )


if __name__ == "__main__":
    unittest.main()
