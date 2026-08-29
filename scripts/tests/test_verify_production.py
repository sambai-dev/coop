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

    def test_attested_result_downloads_remain_exact_and_digest_bound(self) -> None:
        job_id = "job-1"
        receipt_sha256 = "a" * 64
        result = {
            "status": "succeeded",
            "exit_code": 0,
            "stdout": "hello",
            "stderr": "",
            "truncated": False,
            "violations": [],
        }
        artifact = json.dumps(
            {
                "schema_version": 1,
                "job_id": job_id,
                "receipt_sha256": receipt_sha256,
                **result,
            },
            separators=(",", ":"),
        ).encode()
        envelope = json.dumps(
            {
                "payloadType": "application/vnd.in-toto+json",
                "payload": "e30=",
                "signatures": [{"keyid": "sha256:key", "sig": "AA=="}],
            },
            separators=(",", ":"),
        ).encode()
        artifact_digest = hashlib.sha256(artifact).hexdigest()
        envelope_digest = hashlib.sha256(envelope).hexdigest()
        detail = {
            "receipt_sha256": receipt_sha256,
            "attestation": {
                "available": True,
                "key_id": "sha256:key",
                "receipt_sha256": receipt_sha256,
                "result_sha256": artifact_digest,
                "result_size_bytes": len(artifact),
                "envelope_sha256": envelope_digest,
                "envelope_size_bytes": len(envelope),
                "envelope_url": f"/v1/jobs/{job_id}/attestation",
                "result_artifact_url": f"/v1/jobs/{job_id}/result-artifact",
            },
        }

        class FakeApi:
            def request(self, method: str, path: str):  # pragma: no cover - poll fallback
                raise AssertionError((method, path))

            def request_bytes(self, path: str, *, accept: str, max_bytes: int):
                del max_bytes
                if path.endswith("/attestation"):
                    self.assert_accept = accept
                    return envelope, {
                        "content-type": "application/vnd.dsse.envelope.v1+json",
                        "content-length": str(len(envelope)),
                        "x-content-sha256": envelope_digest,
                    }
                return artifact, {
                    "content-type": "application/vnd.coop.execution-result.v1+json",
                    "content-length": str(len(artifact)),
                    "x-content-sha256": artifact_digest,
                }

        status = verify.verify_attested_result(
            FakeApi(), job_id, detail, result, {"key_id": "sha256:key"}
        )
        self.assertEqual(status["result_sha256"], artifact_digest)

        broken = json.loads(json.dumps(detail))
        broken["attestation"]["result_sha256"] = "0" * 64
        with self.assertRaisesRegex(verify.VerificationError, "result artifact digest mismatch"):
            verify.verify_attested_result(
                FakeApi(), job_id, broken, result, {"key_id": "sha256:key"}
            )


if __name__ == "__main__":
    unittest.main()
