from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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
        tenant = "tenant-a"
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
                "tenant": tenant,
                "receipt_sha256": receipt_sha256,
                **result,
            },
            separators=(",", ":"),
        ).encode()
        statement = json.dumps(
            {
                "predicate": {
                    "executionId": job_id,
                    "tenant": tenant,
                }
            },
            separators=(",", ":"),
        ).encode()
        envelope = json.dumps(
            {
                "payloadType": "application/vnd.in-toto+json",
                "payload": base64.b64encode(statement).decode(),
                "signatures": [{"keyid": "sha256:key", "sig": "AA=="}],
            },
            separators=(",", ":"),
        ).encode()
        artifact_digest = hashlib.sha256(artifact).hexdigest()
        envelope_digest = hashlib.sha256(envelope).hexdigest()
        detail = {
            "tenant": tenant,
            "receipt_sha256": receipt_sha256,
            "attestation": {
                "available": True,
                "tenant": tenant,
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

        class FakeOfflineVerifier:
            def __init__(self) -> None:
                self.calls: list[tuple[bytes, bytes]] = []

            def verify(self, exact_envelope: bytes, exact_artifact: bytes, **policy):
                self.calls.append((exact_envelope, exact_artifact))
                assert policy == {
                    "execution_id": job_id,
                    "tenant": tenant,
                    "subject_name": f"coop://jobs/{job_id}/result",
                    "media_type": "application/vnd.coop.execution-result.v1+json",
                    "outcome": "succeeded",
                    "expected_key_id": "sha256:key",
                }
                return {"verified_key_ids": ["sha256:key"]}

        offline = FakeOfflineVerifier()
        status = verify.verify_attested_result(
            FakeApi(),
            job_id,
            tenant,
            detail,
            result,
            {"key_id": "sha256:key"},
            offline,
        )
        self.assertEqual(status["result_sha256"], artifact_digest)
        self.assertEqual(status["verified_key_ids"], ["sha256:key"])
        self.assertEqual(offline.calls, [(envelope, artifact)])

        broken = json.loads(json.dumps(detail))
        broken["attestation"]["result_sha256"] = "0" * 64
        with self.assertRaisesRegex(verify.VerificationError, "result artifact digest mismatch"):
            verify.verify_attested_result(
                FakeApi(),
                job_id,
                tenant,
                broken,
                result,
                {"key_id": "sha256:key"},
                offline,
            )

        wrong_tenant = json.loads(json.dumps(detail))
        wrong_tenant["attestation"]["tenant"] = "tenant-b"
        with self.assertRaisesRegex(verify.VerificationError, "metadata tenant differs"):
            verify.verify_attested_result(
                FakeApi(),
                job_id,
                tenant,
                wrong_tenant,
                result,
                {"key_id": "sha256:key"},
                offline,
            )

    def test_offline_verifier_passes_exact_bytes_and_explicit_pin_to_cli(self) -> None:
        envelope = b'{"payload":"exact envelope bytes"}\n'
        subject = b'{"status":"succeeded","exact":true}\n'
        key_id = "sha256:pinned-key"
        with tempfile.TemporaryDirectory() as raw_temp:
            public_key = Path(raw_temp) / "operator-pinned.pem"
            public_key.write_text(
                "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA"
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n"
                "-----END PUBLIC KEY-----\n",
                encoding="utf-8",
                newline="\n",
            )
            packaged_binary = str((Path(raw_temp) / "rookhold-verify.exe").resolve())
            runner = verify.OfflineVerifier(
                str(public_key.resolve()), binary=packaged_binary
            )

            def run_cli(command, **kwargs):
                self.assertEqual(command[0:2], [packaged_binary, "verify"])
                self.assertIs(kwargs["stdin"], subprocess.DEVNULL)
                envelope_path = Path(command[command.index("--envelope") + 1])
                subject_path = Path(command[command.index("--subject") + 1])
                pinned_path = Path(command[command.index("--public-key") + 1])
                self.assertEqual(envelope_path.read_bytes(), envelope)
                self.assertEqual(subject_path.read_bytes(), subject)
                self.assertEqual(pinned_path, public_key.resolve())
                self.assertEqual(
                    command[command.index("--tenant") + 1],
                    "tenant-exact",
                )
                self.assertEqual(
                    command[command.index("--subject-name") + 1],
                    "coop://jobs/job-exact/result",
                )
                output = {
                    "verified": True,
                    "execution_id": "job-exact",
                    "tenant": "tenant-exact",
                    "subject_name": "coop://jobs/job-exact/result",
                    "subject_media_type": "application/vnd.coop.execution-result.v1+json",
                    "subject_sha256": hashlib.sha256(subject).hexdigest(),
                    "subject_size_bytes": len(subject),
                    "outcome": "succeeded",
                    "event_chain_complete": True,
                    "verified_key_ids": [key_id],
                }
                return subprocess.CompletedProcess(
                    command, 0, json.dumps(output).encode(), b""
                )

            with mock.patch.object(verify.subprocess, "run", side_effect=run_cli):
                output = runner.verify(
                    envelope,
                    subject,
                    execution_id="job-exact",
                    tenant="tenant-exact",
                    subject_name="coop://jobs/job-exact/result",
                    media_type="application/vnd.coop.execution-result.v1+json",
                    outcome="succeeded",
                    expected_key_id=key_id,
                )
        self.assertEqual(output["verified_key_ids"], [key_id])

    def test_offline_verifier_fails_closed_on_bad_signature_or_missing_pin(self) -> None:
        with self.assertRaisesRegex(
            verify.VerificationError, "ROOKHOLD_VERIFY_PUBLIC_KEY_FILE is required"
        ):
            verify.OfflineVerifier("")

        with tempfile.TemporaryDirectory() as raw_temp:
            public_key = Path(raw_temp) / "operator-pinned.pem"
            public_key.write_text("test public key", encoding="utf-8")
            packaged_binary = str((Path(raw_temp) / "rookhold-verify.exe").resolve())
            runner = verify.OfflineVerifier(
                str(public_key.resolve()), binary=packaged_binary
            )
            rejected = subprocess.CompletedProcess(
                [packaged_binary], 1, b"", b"signature verification failed"
            )
            with (
                mock.patch.object(verify.subprocess, "run", return_value=rejected),
                self.assertRaisesRegex(
                    verify.VerificationError, "signature verification failed"
                ),
            ):
                runner.verify(
                    b"{}",
                    b"{}",
                    execution_id="job-1",
                    tenant="tenant-a",
                    subject_name="coop://jobs/job-1/result",
                    media_type="application/vnd.coop.execution-result.v1+json",
                    outcome="succeeded",
                    expected_key_id="sha256:key",
                )

    def test_container_verifier_uses_packaged_entrypoint_without_a_daemon(self) -> None:
        subject = b"exact subject"
        key_id = "sha256:container-pin"
        with tempfile.TemporaryDirectory() as raw_temp:
            public_key = Path(raw_temp) / "operator-pinned.pem"
            public_key.write_text("test public key", encoding="utf-8")
            runner = verify.OfflineVerifier(
                str(public_key.resolve()),
                container_image="sha256:" + "a" * 64,
                docker_binary="/usr/bin/docker",
            )

            def run_container(command, **_kwargs):
                self.assertEqual(command[0:2], ["/usr/bin/docker", "run"])
                self.assertIn("--network=none", command)
                self.assertIn("--read-only", command)
                self.assertIn("--cap-drop=ALL", command)
                self.assertIn("--security-opt=no-new-privileges", command)
                self.assertIn(
                    "ROOKHOLD_VERIFY_PUBLIC_KEY_SOURCE=/run/rookhold-bootstrap/"
                    "trusted-attestation-public-key.pem",
                    command,
                )
                image_index = command.index("sha256:" + "a" * 64)
                self.assertEqual(
                    command[image_index + 1 : image_index + 3],
                    ["/usr/local/bin/rookhold-verify", "verify"],
                )
                self.assertEqual(
                    command[command.index("--tenant") + 1],
                    "tenant-container",
                )
                output = {
                    "verified": True,
                    "execution_id": "job-container",
                    "tenant": "tenant-container",
                    "subject_name": "coop://jobs/job-container/result",
                    "subject_media_type": "application/vnd.coop.execution-result.v1+json",
                    "subject_sha256": hashlib.sha256(subject).hexdigest(),
                    "subject_size_bytes": len(subject),
                    "outcome": "succeeded",
                    "event_chain_complete": True,
                    "verified_key_ids": [key_id],
                }
                return subprocess.CompletedProcess(
                    command, 0, json.dumps(output).encode(), b""
                )

            with mock.patch.object(
                verify.subprocess, "run", side_effect=run_container
            ):
                runner.verify(
                    b"exact envelope",
                    subject,
                    execution_id="job-container",
                    tenant="tenant-container",
                    subject_name="coop://jobs/job-container/result",
                    media_type="application/vnd.coop.execution-result.v1+json",
                    outcome="succeeded",
                    expected_key_id=key_id,
                )

    def test_offline_verifier_rejects_wrong_authenticated_tenant_output(self) -> None:
        subject = b"exact subject"
        with tempfile.TemporaryDirectory() as raw_temp:
            public_key = Path(raw_temp) / "operator-pinned.pem"
            public_key.write_text("test public key", encoding="utf-8")
            packaged_binary = str((Path(raw_temp) / "rookhold-verify.exe").resolve())
            runner = verify.OfflineVerifier(
                str(public_key.resolve()), binary=packaged_binary
            )
            output = {
                "verified": True,
                "execution_id": "job-tenant",
                "tenant": "tenant-b",
                "subject_name": "coop://jobs/job-tenant/result",
                "subject_media_type": "application/vnd.coop.execution-result.v1+json",
                "subject_sha256": hashlib.sha256(subject).hexdigest(),
                "subject_size_bytes": len(subject),
                "outcome": "succeeded",
                "event_chain_complete": True,
                "verified_key_ids": ["sha256:key"],
            }
            completed = subprocess.CompletedProcess(
                [packaged_binary], 0, json.dumps(output).encode(), b""
            )
            with (
                mock.patch.object(verify.subprocess, "run", return_value=completed),
                self.assertRaisesRegex(verify.VerificationError, "tenant differs"),
            ):
                runner.verify(
                    b"exact envelope",
                    subject,
                    execution_id="job-tenant",
                    tenant="tenant-a",
                    subject_name="coop://jobs/job-tenant/result",
                    media_type="application/vnd.coop.execution-result.v1+json",
                    outcome="succeeded",
                    expected_key_id="sha256:key",
                )

    def test_container_verifier_rejects_mutable_image_tags(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            public_key = Path(raw_temp) / "operator-pinned.pem"
            public_key.write_text("test public key", encoding="utf-8")
            with self.assertRaisesRegex(
                verify.VerificationError, "immutable sha256 image ID"
            ):
                verify.OfflineVerifier(
                    str(public_key.resolve()), container_image="coop:latest"
                )


if __name__ == "__main__":
    unittest.main()
