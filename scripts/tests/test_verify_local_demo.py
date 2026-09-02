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

    def write_receipt(self, runs_root: Path) -> None:
        receipt = runs_root / self.JOB_ID / "receipt.json"
        receipt.parent.mkdir(parents=True)
        receipt.write_text("{}", encoding="utf-8")

    def test_accepts_truthful_local_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            self.write_receipt(root)
            MODULE.verify(self.payload(root), expected_runs_root=root)

    def test_rejects_missing_or_non_string_result_job_id(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            for value in [None, 7]:
                with self.subTest(value=value):
                    payload = self.payload(root)
                    payload["result"] = {"status": "succeeded", "stdout": "42", "job_id": value}
                    with self.assertRaisesRegex(AssertionError, "result omits its job_id"):
                        MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_non_host_networking_independently(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            payload = self.payload(root)
            payload["receipt"] = {
                "networking": "disabled",
                "isolation_class": "none",
                "job_id": self.JOB_ID,
            }
            with self.assertRaisesRegex(AssertionError, "host networking"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_mismatched_receipt_job_id(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            payload = self.payload(root)
            payload["receipt"] = {
                "networking": "host",
                "isolation_class": "none",
                "job_id": self.OTHER_JOB_ID,
            }
            with self.assertRaisesRegex(AssertionError, "job IDs differ"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_non_uuidv7_job_id(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            payload = self.payload(root)
            uuidv4 = "123e4567-e89b-42d3-a456-426614174000"
            payload["result"] = {"status": "succeeded", "stdout": "42", "job_id": uuidv4}
            payload["receipt"] = {
                "networking": "host",
                "isolation_class": "none",
                "job_id": uuidv4,
            }
            payload["receipt_path"] = str(root / uuidv4 / "receipt.json")
            with self.assertRaisesRegex(AssertionError, "not UUIDv7"):
                MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

    def test_rejects_each_malformed_receipt_path_shape(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / ".rookhold" / "runs"
            cases = {
                "missing": None,
                "empty": "",
                "nul": "receipt\x00.json",
                "unexpected": str(root / self.JOB_ID / "result.json"),
            }
            for name, value in cases.items():
                with self.subTest(name=name):
                    payload = self.payload(root)
                    if value is None:
                        payload.pop("receipt_path")
                    else:
                        payload["receipt_path"] = value
                    expected = "did not save" if name == "missing" else "malformed|unexpected"
                    with self.assertRaisesRegex(AssertionError, expected):
                        MODULE.verify(payload, expected_runs_root=root, require_receipt_file=False)

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
