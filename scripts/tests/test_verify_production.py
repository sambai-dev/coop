from __future__ import annotations

import hashlib
import importlib.util
import json
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).resolve().parents[1] / "verify-production.py"
SPEC = importlib.util.spec_from_file_location("coop_verify_production", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
verify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify)


def enforcement() -> dict[str, bool]:
    return {
        "wall_seconds": True,
        "cpu_seconds": True,
        "mem_mb": True,
        "max_pids": True,
        "max_file_mb": True,
    }


def namespace_posture() -> dict[str, object]:
    return {
        "backend": "namespaces+cgroups-v2+private-rootfs",
        "isolation_class": "linux-shared-kernel",
        "isolated": True,
        "private_rootfs": True,
        "dedicated_bootstrap": True,
        "seccomp": True,
        "networking": "disabled",
        "network_allowed": False,
        "limit_enforcement": enforcement(),
    }


def gvisor_posture(*, terminal: bool = False) -> dict[str, object]:
    posture: dict[str, object] = {
        "backend": "gvisor-oci",
        "isolation_class": "gvisor-application-kernel",
        "isolated": True,
        "private_rootfs": True,
        "dedicated_bootstrap": True,
        "seccomp": False,
        "networking": "disabled",
        "network_allowed": False,
        "limit_enforcement": enforcement(),
    }
    if terminal:
        posture.update(
            {
                "runtime_sha256": "a" * 64,
                "rootfs_sha256": "b" * 64,
                "config_sha256": "c" * 64,
            }
        )
    return posture


class IsolationVerificationTests(unittest.TestCase):
    def test_server_isolation_partial_order_is_reproduced_exactly(self) -> None:
        self.assertTrue(
            verify.isolation_satisfies(
                "gvisor-application-kernel", "linux-shared-kernel"
            )
        )
        self.assertTrue(verify.isolation_satisfies("confidential-vm", "hardware-vm"))
        self.assertFalse(
            verify.isolation_satisfies(
                "linux-shared-kernel", "gvisor-application-kernel"
            )
        )
        self.assertTrue(
            verify.isolation_satisfies("wasm-capability", "wasm-capability")
        )
        self.assertFalse(
            verify.isolation_satisfies("wasm-capability", "linux-shared-kernel")
        )
        self.assertFalse(verify.isolation_satisfies("hardware-vm", "wasm-capability"))

    def test_minimum_isolation_requires_an_exact_server_value(self) -> None:
        self.assertEqual(
            verify.parse_minimum_isolation("gvisor-application-kernel"),
            "gvisor-application-kernel",
        )
        for invalid in ["Gvisor-Application-Kernel", "container", ""]:
            with (
                self.subTest(invalid=invalid),
                self.assertRaises(verify.VerificationError),
            ):
                verify.parse_minimum_isolation(invalid)

    def test_namespace_requires_its_provider_specific_controls(self) -> None:
        self.assertEqual(
            verify.require_execution(
                namespace_posture(), "namespace", "linux-shared-kernel"
            ),
            "linux-shared-kernel",
        )
        broken = namespace_posture()
        broken["seccomp"] = False
        with self.assertRaisesRegex(verify.VerificationError, "seccomp"):
            verify.require_execution(broken, "namespace", "linux-shared-kernel")

    def test_gvisor_uses_runtime_digests_not_namespace_seccomp(self) -> None:
        self.assertEqual(
            verify.require_execution(
                gvisor_posture(terminal=True),
                "gvisor",
                "linux-shared-kernel",
                terminal_evidence=True,
            ),
            "gvisor-application-kernel",
        )
        missing_digest = gvisor_posture(terminal=True)
        missing_digest["config_sha256"] = None
        with self.assertRaisesRegex(verify.VerificationError, "config_sha256"):
            verify.require_execution(
                missing_digest,
                "gvisor",
                "gvisor-application-kernel",
                terminal_evidence=True,
            )
        with self.assertRaises(verify.VerificationError):
            verify.require_execution(gvisor_posture(), "gvisor", "hardware-vm")

    def test_receipt_integrity_check_remains_provider_aware(self) -> None:
        receipt = {
            **gvisor_posture(terminal=True),
            "bootstrap_ready": True,
            "minimum_isolation": "gvisor-application-kernel",
            "evidence_complete": True,
            "event_chain": {"complete": True, "events": 3, "head": "d" * 64},
            "output": {
                "truncated": False,
                "stdout_sha256": "e" * 64,
                "stderr_sha256": "f" * 64,
            },
        }
        canonical = json.dumps(
            receipt, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        )
        digest = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
        receipt["receipt_sha256"] = digest
        detail = {"receipt": receipt, "receipt_sha256": digest}
        verify.verify_receipt(detail, "gvisor-application-kernel")


if __name__ == "__main__":
    unittest.main()
