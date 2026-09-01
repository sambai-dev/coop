from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "verify-local-demo.py"
SPEC = importlib.util.spec_from_file_location("verify_local_demo", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class VerifyLocalDemoTests(unittest.TestCase):
    def payload(self) -> dict[str, object]:
        return {
            "result": {"status": "succeeded", "stdout": "42"},
            "receipt": {"networking": "host", "isolation_class": "none"},
            "receipt_path": ".rookhold/runs/example/receipt.json",
        }

    def test_accepts_truthful_local_snapshot(self) -> None:
        MODULE.verify(self.payload(), require_receipt_file=False)

    def test_rejects_false_gvisor_claim(self) -> None:
        payload = self.payload()
        payload["receipt"] = {
            "networking": "disabled",
            "isolation_class": "gvisor-application-kernel",
        }
        with self.assertRaisesRegex(AssertionError, "host networking"):
            MODULE.verify(payload, require_receipt_file=False)


if __name__ == "__main__":
    unittest.main()
