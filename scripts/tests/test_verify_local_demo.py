from __future__ import annotations

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "verify-local-demo.py"
SPEC = importlib.util.spec_from_file_location("verify_local_demo", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class VerifyLocalDemoTests(unittest.TestCase):
    JOB_ID = "01900000-0000-7000-8000-000000000001"
    OTHER_JOB_ID = "01900000-0000-7000-8000-000000000002"

    def payload(self, runs_root: Path) -> dict[str, object]:
        return {
            "result": {"status": "succeeded", "stdout": "42", "job_id": self.JOB_ID},
            "receipt": {
                "networking": "host",
                "isolation_class": "none",
                "job_id": self.JOB_ID,
            },
            "receipt_path": str(runs_root / self.JOB_ID / "receipt.json"),
        }

    def test_accepts_truthful_local_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            MODULE.verify(
                self.payload(root), expected_runs_root=root, require_receipt_file=False
            )

    def test_rejects_false_gvisor_claim(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            payload = self.payload(root)
            payload["receipt"] = {
                "networking": "host",
                "isolation_class": "gvisor-application-kernel",
                "job_id": self.JOB_ID,
            }
            with self.assertRaisesRegex(AssertionError, "isolation none"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_out_of_tree_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / ".rookhold" / "runs"
            payload = self.payload(root)
            payload["receipt_path"] = str(base / "outside" / "receipt.json")
            with self.assertRaisesRegex(AssertionError, "escapes"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_stale_receipt_from_another_job(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            payload = self.payload(root)
            payload["receipt_path"] = str(root / self.OTHER_JOB_ID / "receipt.json")
            with self.assertRaisesRegex(AssertionError, "stale"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_traversal_receipt_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            payload = self.payload(root)
            payload["receipt_path"] = str(
                root / self.JOB_ID / ".." / self.OTHER_JOB_ID / "receipt.json"
            )
            with self.assertRaisesRegex(AssertionError, "traversal"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_symlink_escape(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / ".rookhold" / "runs"
            job_dir = root / self.JOB_ID
            job_dir.mkdir(parents=True)
            outside = base / "outside-receipt.json"
            outside.write_text("{}", encoding="utf-8")
            link = job_dir / "receipt.json"
            try:
                os.symlink(outside, link)
            except OSError as error:
                self.skipTest(f"symlink unavailable: {error}")
            payload = self.payload(root)
            with self.assertRaisesRegex(AssertionError, "escapes"):
                MODULE.verify(payload, expected_runs_root=root)


if __name__ == "__main__":
    unittest.main()
