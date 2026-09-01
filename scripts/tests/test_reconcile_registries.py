from __future__ import annotations

import hashlib
import io
import json
import runpy
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError

ROOT = Path(__file__).resolve().parents[2]
MODULE = runpy.run_path(str(ROOT / "scripts" / "reconcile-registries.py"))


class Response(io.BytesIO):
    def __init__(self, content: bytes, url: str) -> None:
        super().__init__(content)
        self._url = url

    def geturl(self) -> str:
        return self._url


def json_response(value: object, url: str) -> Response:
    return Response(json.dumps(value).encode(), url)


class RegistryReconciliationTests(unittest.TestCase):
    VERSION = "0.8.0"

    def make_dist(self, directory: str) -> tuple[Path, dict[str, bytes]]:
        dist = Path(directory)
        content = {
            f"rookhold-{self.VERSION}-py3-none-any.whl": b"wheel-bytes",
            f"rookhold-{self.VERSION}.tar.gz": b"sdist-bytes",
            f"rookhold-{self.VERSION}.tgz": b"npm-bytes",
        }
        for name, value in content.items():
            (dist / name).write_bytes(value)
        return dist, content

    def test_missing_version_is_safe_to_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            dist, _ = self.make_dist(directory)
            missing = HTTPError("https://registry.example", 404, "missing", {}, None)
            with patch.dict(MODULE["registry_state"].__globals__, {"urlopen": lambda *_a, **_k: (_ for _ in ()).throw(missing)}):
                self.assertEqual(MODULE["registry_state"]("pypi", self.VERSION, dist), "missing")
                self.assertEqual(MODULE["registry_state"]("npm", self.VERSION, dist), "missing")

    def test_pypi_requires_the_exact_file_set_and_digests(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            dist, content = self.make_dist(directory)
            files = [
                {"filename": name, "digests": {"sha256": hashlib.sha256(value).hexdigest()}}
                for name, value in content.items()
                if name.endswith((".whl", ".tar.gz"))
            ]
            url = f"https://pypi.org/pypi/rookhold/{self.VERSION}/json"
            with patch.dict(
                MODULE["registry_state"].__globals__,
                {"urlopen": lambda *_a, **_k: json_response({"urls": files}, url)},
            ):
                self.assertEqual(MODULE["registry_state"]("pypi", self.VERSION, dist), "exact")
                files[0]["digests"]["sha256"] = "0" * 64
                with self.assertRaisesRegex(ValueError, "digest differs"):
                    MODULE["registry_state"]("pypi", self.VERSION, dist)

    def test_npm_downloads_and_compares_the_actual_tarball(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            dist, content = self.make_dist(directory)
            metadata_url = f"https://registry.npmjs.org/rookhold/{self.VERSION}"
            tarball_url = f"https://registry.npmjs.org/rookhold/-/rookhold-{self.VERSION}.tgz"

            def open_exact(request, **_kwargs):
                if request.full_url == metadata_url:
                    return json_response(
                        {
                            "name": "rookhold",
                            "version": self.VERSION,
                            "dist": {"tarball": tarball_url},
                        },
                        metadata_url,
                    )
                return Response(content[f"rookhold-{self.VERSION}.tgz"], tarball_url)

            with patch.dict(MODULE["registry_state"].__globals__, {"urlopen": open_exact}):
                self.assertEqual(MODULE["registry_state"]("npm", self.VERSION, dist), "exact")

                def open_mismatch(request, **_kwargs):
                    response = open_exact(request)
                    return Response(b"different", tarball_url) if request.full_url == tarball_url else response

                with patch.dict(MODULE["registry_state"].__globals__, {"urlopen": open_mismatch}):
                    with self.assertRaisesRegex(ValueError, "digest differs"):
                        MODULE["registry_state"]("npm", self.VERSION, dist)


if __name__ == "__main__":
    unittest.main()
