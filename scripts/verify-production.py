#!/usr/bin/env python3
"""Verify a running Coop deployment through its authenticated public API."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Tuple, cast

MAX_JSON_BYTES = 4 * 1024 * 1024
MAX_ATTESTATION_BYTES = 2 * 1024 * 1024
MAX_RESULT_ARTIFACT_BYTES = 16 * 1024 * 1024
ISOLATION_CLASSES = frozenset(
    {
        "none",
        "linux-shared-kernel",
        "gvisor-application-kernel",
        "wasm-capability",
        "hardware-vm",
        "confidential-vm",
    }
)
DEFAULT_MINIMUM_ISOLATION = "linux-shared-kernel"
RESULT_ARTIFACT_MEDIA_TYPE = "application/vnd.coop.execution-result.v1+json"
IMAGE_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
CANARIES = {
    "python": (
        'print("coop-production-canary-python")',
        "coop-production-canary-python",
    ),
    "node": (
        'console.log("coop-production-canary-node")',
        "coop-production-canary-node",
    ),
    "bash": (
        'printf "%s\\n" "coop-production-canary-bash"',
        "coop-production-canary-bash",
    ),
}


class VerificationError(RuntimeError):
    pass


class OfflineVerifier:
    """Run the packaged verifier against exact downloaded artifact bytes."""

    def __init__(
        self,
        public_key_file: str,
        *,
        binary: str = "coop-verify",
        container_image: Optional[str] = None,
        docker_binary: str = "docker",
    ) -> None:
        if not public_key_file.strip():
            raise VerificationError(
                "COOP_VERIFY_PUBLIC_KEY_FILE is required; obtain and pin the "
                "Ed25519 public key through an authenticated out-of-band channel"
            )
        public_key = Path(public_key_file)
        if not public_key.is_absolute():
            raise VerificationError("COOP_VERIFY_PUBLIC_KEY_FILE must be absolute")
        try:
            metadata = public_key.lstat()
        except OSError as exc:
            raise VerificationError(
                f"cannot inspect COOP_VERIFY_PUBLIC_KEY_FILE: {exc}"
            ) from None
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise VerificationError(
                "COOP_VERIFY_PUBLIC_KEY_FILE must be a non-symlink regular file"
            )
        self.public_key_file = public_key
        self.binary = binary.strip()
        self.container_image = container_image.strip() if container_image else None
        self.docker_binary = docker_binary.strip()
        if self.container_image is not None:
            if not IMAGE_ID.fullmatch(self.container_image):
                raise VerificationError(
                    "COOP_VERIFY_CONTAINER_IMAGE must be an immutable sha256 image ID"
                )
            if not self.docker_binary:
                raise VerificationError("COOP_VERIFY_DOCKER_BIN must not be empty")
        elif not self.binary or not Path(self.binary).is_absolute():
            raise VerificationError(
                "COOP_VERIFY_BIN must be the absolute path to the packaged verifier"
            )

    @classmethod
    def from_environment(cls) -> "OfflineVerifier":
        return cls(
            os.environ.get("COOP_VERIFY_PUBLIC_KEY_FILE", ""),
            binary=os.environ.get("COOP_VERIFY_BIN", ""),
            container_image=os.environ.get("COOP_VERIFY_CONTAINER_IMAGE") or None,
            docker_binary=os.environ.get("COOP_VERIFY_DOCKER_BIN", "docker"),
        )

    def verify(
        self,
        envelope: bytes,
        subject: bytes,
        *,
        execution_id: str,
        tenant: str,
        subject_name: str,
        media_type: str,
        outcome: str,
        expected_key_id: str,
    ) -> Dict[str, Any]:
        with tempfile.TemporaryDirectory(prefix="coop-production-verify-") as raw_temp:
            temporary = Path(raw_temp)
            envelope_path = temporary / "attestation.dsse.json"
            subject_path = temporary / "result-artifact.json"
            envelope_path.write_bytes(envelope)
            subject_path.write_bytes(subject)
            envelope_path.chmod(0o600)
            subject_path.chmod(0o600)

            if self.container_image is None:
                command = [
                    self.binary,
                    "verify",
                    "--envelope",
                    str(envelope_path),
                    "--subject",
                    str(subject_path),
                    "--public-key",
                    str(self.public_key_file),
                ]
            else:
                for path in [temporary, self.public_key_file]:
                    require(
                        "," not in str(path),
                        "Docker-backed verification paths must not contain commas",
                    )
                command = [
                    self.docker_binary,
                    "run",
                    "--rm",
                    "--network=none",
                    "--read-only",
                    "--cap-drop=ALL",
                    "--cap-add=DAC_OVERRIDE",
                    "--cap-add=CHOWN",
                    "--security-opt=no-new-privileges",
                    "--user=0:0",
                    "--tmpfs=/run/coop-secrets:rw,noexec,nosuid,nodev,mode=0700,uid=0,gid=0",
                    "--mount",
                    f"type=bind,src={temporary},dst=/verify-input,readonly",
                    "--mount",
                    "type=bind,src="
                    f"{self.public_key_file},"
                    "dst=/run/coop-bootstrap/trusted-attestation-public-key.pem,readonly",
                    "--env",
                    "COOP_VERIFY_PUBLIC_KEY_SOURCE=/run/coop-bootstrap/trusted-attestation-public-key.pem",
                    "--env",
                    "COOP_VERIFY_PUBLIC_KEY_FILE=/run/coop-secrets/trusted-attestation-public-key.pem",
                    self.container_image,
                    "/usr/local/bin/coop-verify",
                    "verify",
                    "--envelope",
                    "/verify-input/attestation.dsse.json",
                    "--subject",
                    "/verify-input/result-artifact.json",
                    "--public-key",
                    "/run/coop-secrets/trusted-attestation-public-key.pem",
                ]
            command.extend(
                [
                    "--tenant",
                    tenant,
                    "--subject-name",
                    subject_name,
                    "--media-type",
                    media_type,
                ]
            )
            try:
                completed = subprocess.run(
                    command,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    check=False,
                    timeout=30,
                )
            except (OSError, subprocess.TimeoutExpired) as exc:
                raise VerificationError(f"coop-verify could not run: {exc}") from None
            if completed.returncode != 0:
                detail = completed.stderr[:64 * 1024].decode(
                    "utf-8", errors="replace"
                )
                raise VerificationError(
                    f"coop-verify rejected the signed result: {detail.strip()}"
                )
            if len(completed.stdout) > 64 * 1024:
                raise VerificationError("coop-verify output exceeded 65536 bytes")
            try:
                output = json.loads(completed.stdout.decode("utf-8"))
            except (UnicodeError, json.JSONDecodeError) as exc:
                raise VerificationError(
                    f"coop-verify returned invalid JSON: {exc}"
                ) from None
            require(isinstance(output, dict), "coop-verify output was not an object")
            verified_key_ids = output.get("verified_key_ids")
            require(output.get("verified") is True, "coop-verify did not report success")
            require(
                output.get("execution_id") == execution_id,
                "coop-verify execution ID differs from the job",
            )
            require(
                output.get("tenant") == tenant,
                "coop-verify tenant differs from policy",
            )
            require(
                output.get("subject_name") == subject_name,
                "coop-verify subject name differs from policy",
            )
            require(
                output.get("subject_media_type") == media_type,
                "coop-verify subject media type differs from policy",
            )
            require(
                output.get("subject_sha256") == hashlib.sha256(subject).hexdigest(),
                "coop-verify subject digest differs from the exact result bytes",
            )
            require(
                output.get("subject_size_bytes") == len(subject),
                "coop-verify subject size differs from the exact result bytes",
            )
            require(
                output.get("outcome") == outcome,
                "coop-verify outcome differs from the API result",
            )
            require(
                output.get("event_chain_complete") is True,
                "coop-verify did not authenticate a complete event chain",
            )
            require(
                isinstance(verified_key_ids, list)
                and bool(verified_key_ids)
                and all(isinstance(value, str) for value in verified_key_ids),
                "coop-verify returned no verified key IDs",
            )
            require(
                expected_key_id in verified_key_ids,
                "the server's attestation key ID was not verified by the pinned key",
            )
            return cast(Dict[str, Any], output)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


class Api:
    def __init__(self, base_url: str, key: str) -> None:
        parts = urllib.parse.urlsplit(base_url)
        if parts.scheme not in {"http", "https"} or not parts.netloc:
            raise VerificationError("COOP_VERIFY_BASE_URL must be an absolute HTTP URL")
        if parts.username or parts.password or parts.query or parts.fragment:
            raise VerificationError(
                "COOP_VERIFY_BASE_URL must not contain credentials, a query, "
                "or fragment"
            )
        if parts.scheme == "http" and parts.hostname not in {"127.0.0.1", "localhost"}:
            raise VerificationError(
                "plain HTTP verification is allowed only on loopback; use HTTPS "
                "remotely"
            )
        if not key.strip():
            raise VerificationError("COOP_CLIENT_KEY is required")
        self.base_url = urllib.parse.urlunsplit(
            (parts.scheme, parts.netloc, parts.path.rstrip("/"), "", "")
        )
        self.key = key
        self._opener = urllib.request.build_opener(_NoRedirect())

    def request(
        self,
        method: str,
        path: str,
        payload: Optional[Mapping[str, Any]] = None,
        *,
        timeout: float = 10,
    ) -> Dict[str, Any]:
        body = None
        headers = {
            "Accept": "application/json",
            "Authorization": f"Bearer {self.key}",
            "User-Agent": "coop-production-verifier/1",
        }
        if payload is not None:
            body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            self.base_url + "/" + path.lstrip("/"),
            data=body,
            method=method,
            headers=headers,
        )
        try:
            with self._opener.open(request, timeout=timeout) as response:
                raw = response.read(MAX_JSON_BYTES + 1)
        except urllib.error.HTTPError as exc:
            raw = exc.read(64 * 1024)
            detail = raw.decode("utf-8", errors="replace")
            raise VerificationError(
                f"{method} {path} returned HTTP {exc.code}: {detail}"
            ) from None
        except (OSError, TimeoutError, urllib.error.URLError) as exc:
            raise VerificationError(f"{method} {path} failed: {exc}") from None
        if len(raw) > MAX_JSON_BYTES:
            raise VerificationError(f"{method} {path} exceeded {MAX_JSON_BYTES} bytes")
        try:
            value = json.loads(raw.decode("utf-8"))
        except (UnicodeError, json.JSONDecodeError) as exc:
            raise VerificationError(
                f"{method} {path} returned invalid JSON: {exc}"
            ) from None
        if not isinstance(value, dict):
            raise VerificationError(f"{method} {path} did not return a JSON object")
        return cast(Dict[str, Any], value)

    def request_bytes(
        self,
        path: str,
        *,
        accept: str,
        max_bytes: int,
        timeout: float = 10,
    ) -> Tuple[bytes, Mapping[str, str]]:
        request = urllib.request.Request(
            self.base_url + "/" + path.lstrip("/"),
            method="GET",
            headers={
                "Accept": accept,
                "Authorization": f"Bearer {self.key}",
                "User-Agent": "coop-production-verifier/1",
            },
        )
        try:
            with self._opener.open(request, timeout=timeout) as response:
                raw = response.read(max_bytes + 1)
                headers = {name.lower(): value for name, value in response.headers.items()}
        except urllib.error.HTTPError as exc:
            detail = exc.read(64 * 1024).decode("utf-8", errors="replace")
            raise VerificationError(f"GET {path} returned HTTP {exc.code}: {detail}") from None
        except (OSError, TimeoutError, urllib.error.URLError) as exc:
            raise VerificationError(f"GET {path} failed: {exc}") from None
        if len(raw) > max_bytes:
            raise VerificationError(f"GET {path} exceeded {max_bytes} bytes")
        return raw, headers


def require(condition: bool, message: str) -> None:
    if not condition:
        raise VerificationError(message)


def parse_minimum_isolation(raw: str) -> str:
    value = raw.strip()
    require(
        value in ISOLATION_CLASSES,
        "COOP_VERIFY_MINIMUM_ISOLATION must be one of "
        + ",".join(sorted(ISOLATION_CLASSES)),
    )
    return value


def isolation_satisfies(observed: str, minimum: str) -> bool:
    if minimum == "none":
        return observed in ISOLATION_CLASSES
    if minimum == "wasm-capability":
        return observed == "wasm-capability"
    process_classes = [
        "linux-shared-kernel",
        "gvisor-application-kernel",
        "hardware-vm",
        "confidential-vm",
    ]
    if minimum not in process_classes or observed not in process_classes:
        return False
    return process_classes.index(observed) >= process_classes.index(minimum)


def require_sha256(value: Any, message: str) -> None:
    require(
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value),
        message,
    )


def require_limit_enforcement(document: Mapping[str, Any], label: str) -> None:
    enforcement = document.get("limit_enforcement")
    require(isinstance(enforcement, dict), f"{label}: limit enforcement is missing")
    for field in ["wall_seconds", "cpu_seconds", "mem_mb", "max_pids", "max_file_mb"]:
        require(enforcement.get(field) is True, f"{label}: {field} is not enforced")


def require_execution(
    document: Mapping[str, Any],
    label: str,
    minimum_isolation: str,
    *,
    terminal_evidence: bool = False,
) -> str:
    observed = document.get("isolation_class")
    require(
        isinstance(observed, str) and observed in ISOLATION_CLASSES,
        f"{label}: isolation_class is missing or invalid",
    )
    require(
        isolation_satisfies(observed, minimum_isolation),
        f"{label}: isolation class {observed!r} does not satisfy {minimum_isolation!r}",
    )

    if observed == "none":
        require(
            document.get("isolated") is False, f"{label}: none class claimed isolation"
        )
        require(
            document.get("networking") == "host",
            f"{label}: none class hid host networking",
        )
        return observed

    require(document.get("isolated") is True, f"{label}: isolated is not true")
    require(
        document.get("networking") == "disabled", f"{label}: networking is not disabled"
    )
    if "network_allowed" in document:
        require(
            document.get("network_allowed") is False, f"{label}: networking was allowed"
        )
    require_limit_enforcement(document, label)

    backend = document.get("backend")
    if observed == "linux-shared-kernel":
        require(
            backend == "namespaces+cgroups-v2+private-rootfs",
            f"{label}: namespace class has an unexpected backend",
        )
        for field in ["private_rootfs", "dedicated_bootstrap", "seccomp"]:
            require(document.get(field) is True, f"{label}: {field} is not true")
    elif observed == "gvisor-application-kernel":
        require(
            backend == "gvisor-oci", f"{label}: gVisor class has an unexpected backend"
        )
        require(
            document.get("private_rootfs") is True,
            f"{label}: private_rootfs is not true",
        )
        require(
            document.get("dedicated_bootstrap") is True,
            f"{label}: dedicated_bootstrap is not true",
        )
        if terminal_evidence:
            for field in ["runtime_sha256", "rootfs_sha256", "config_sha256"]:
                require_sha256(document.get(field), f"{label}: invalid {field}")
    return observed


def verify_receipt(detail: Mapping[str, Any], minimum_isolation: str) -> None:
    receipt = detail.get("receipt")
    require(isinstance(receipt, dict), "terminal detail has no receipt")
    require(
        receipt.get("bootstrap_ready") is True,
        "receipt did not observe bootstrap readiness",
    )
    require(
        receipt.get("minimum_isolation") == minimum_isolation,
        "receipt minimum isolation differs from the submitted requirement",
    )
    require_execution(
        receipt,
        "receipt",
        minimum_isolation,
        terminal_evidence=True,
    )
    require(receipt.get("evidence_complete") is True, "receipt evidence is incomplete")
    chain = receipt.get("event_chain")
    require(
        isinstance(chain, dict) and chain.get("complete") is True,
        "event chain is incomplete",
    )
    require(
        isinstance(chain.get("events"), int) and chain["events"] > 0,
        "event chain is empty",
    )
    require(
        isinstance(chain.get("head"), str) and len(chain["head"]) == 64,
        "event chain head is invalid",
    )
    output = receipt.get("output")
    require(isinstance(output, dict), "receipt output evidence is missing")
    require(output.get("truncated") is False, "canary output was truncated")
    for field in ["stdout_sha256", "stderr_sha256"]:
        require_sha256(output.get(field), f"invalid {field}")
    recorded = receipt.get("receipt_sha256")
    require(
        recorded == detail.get("receipt_sha256"), "detail and receipt hashes differ"
    )
    unsigned = dict(receipt)
    unsigned.pop("receipt_sha256", None)
    canonical = json.dumps(
        unsigned, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    )
    require(
        hashlib.sha256(canonical.encode("utf-8")).hexdigest() == recorded,
        "receipt hash does not match canonical content",
    )


def verify_attested_result(
    api: Api,
    job_id: str,
    tenant: str,
    detail: Dict[str, Any],
    result: Mapping[str, Any],
    attestation_capabilities: Mapping[str, Any],
    offline_verifier: OfflineVerifier,
) -> Dict[str, Any]:
    encoded_job_id = urllib.parse.quote(job_id, safe="")
    deadline = time.monotonic() + 15
    while True:
        status = detail.get("attestation")
        if isinstance(status, dict) and status.get("available") is True:
            break
        if time.monotonic() >= deadline:
            raise VerificationError(f"{job_id}: signed attestation was not available in time")
        time.sleep(0.1)
        detail = api.request("GET", f"/v1/jobs/{encoded_job_id}")

    status = cast(Dict[str, Any], detail["attestation"])
    require(detail.get("tenant") == tenant, f"{job_id}: job detail tenant differs")
    require(
        status.get("tenant") == tenant,
        f"{job_id}: attestation metadata tenant differs",
    )
    key_id = status.get("key_id")
    require(
        isinstance(key_id, str) and key_id == attestation_capabilities.get("key_id"),
        f"{job_id}: attestation key id differs from capabilities",
    )
    require(
        status.get("receipt_sha256") == detail.get("receipt_sha256"),
        f"{job_id}: attestation is not bound to the terminal receipt",
    )
    envelope_path = status.get("envelope_url")
    result_path = status.get("result_artifact_url")
    require(
        envelope_path == f"/v1/jobs/{job_id}/attestation",
        f"{job_id}: unexpected attestation URL",
    )
    require(
        result_path == f"/v1/jobs/{job_id}/result-artifact",
        f"{job_id}: unexpected result-artifact URL",
    )

    envelope, envelope_headers = api.request_bytes(
        cast(str, envelope_path),
        accept="application/vnd.dsse.envelope.v1+json",
        max_bytes=MAX_ATTESTATION_BYTES,
    )
    artifact, artifact_headers = api.request_bytes(
        cast(str, result_path),
        accept="application/vnd.coop.execution-result.v1+json",
        max_bytes=MAX_RESULT_ARTIFACT_BYTES,
    )
    for label, data, headers, expected_digest, expected_size, expected_type in [
        (
            "attestation envelope",
            envelope,
            envelope_headers,
            status.get("envelope_sha256"),
            status.get("envelope_size_bytes"),
            "application/vnd.dsse.envelope.v1+json",
        ),
        (
            "result artifact",
            artifact,
            artifact_headers,
            status.get("result_sha256"),
            status.get("result_size_bytes"),
            "application/vnd.coop.execution-result.v1+json",
        ),
    ]:
        require_sha256(expected_digest, f"{job_id}: invalid {label} digest")
        actual_digest = hashlib.sha256(data).hexdigest()
        require(actual_digest == expected_digest, f"{job_id}: {label} digest mismatch")
        require(
            headers.get("x-content-sha256") == expected_digest,
            f"{job_id}: {label} response digest header mismatch",
        )
        require(
            headers.get("content-type") == expected_type,
            f"{job_id}: {label} content type mismatch",
        )
        require(expected_size == len(data), f"{job_id}: {label} size mismatch")
        require(
            headers.get("content-length") == str(len(data)),
            f"{job_id}: {label} Content-Length mismatch",
        )

    try:
        envelope_document = json.loads(envelope.decode("utf-8"))
        artifact_document = json.loads(artifact.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise VerificationError(f"{job_id}: attestation bytes are not valid UTF-8 JSON: {exc}") from None
    require(isinstance(envelope_document, dict), f"{job_id}: DSSE envelope is not an object")
    require(
        envelope_document.get("payloadType") == "application/vnd.in-toto+json",
        f"{job_id}: unexpected DSSE payload type",
    )
    payload = envelope_document.get("payload")
    require(isinstance(payload, str), f"{job_id}: DSSE payload is missing")
    try:
        statement_document = json.loads(base64.b64decode(payload, validate=True).decode("utf-8"))
    except (ValueError, UnicodeError, json.JSONDecodeError) as exc:
        raise VerificationError(f"{job_id}: DSSE statement is not valid base64 UTF-8 JSON: {exc}") from None
    require(isinstance(statement_document, dict), f"{job_id}: DSSE statement is not an object")
    predicate = statement_document.get("predicate")
    require(isinstance(predicate, dict), f"{job_id}: DSSE predicate is missing")
    require(
        predicate.get("executionId") == job_id,
        f"{job_id}: attestation predicate job differs",
    )
    require(
        predicate.get("tenant") == tenant,
        f"{job_id}: attestation predicate tenant differs",
    )
    signatures = envelope_document.get("signatures")
    require(
        isinstance(signatures, list)
        and bool(signatures)
        and all(isinstance(signature, dict) for signature in signatures),
        f"{job_id}: DSSE signatures are missing",
    )
    require(isinstance(artifact_document, dict), f"{job_id}: result artifact is not an object")
    require(artifact_document.get("schema_version") == 1, f"{job_id}: result artifact version differs")
    require(artifact_document.get("job_id") == job_id, f"{job_id}: result artifact job differs")
    require(artifact_document.get("tenant") == tenant, f"{job_id}: result artifact tenant differs")
    require(
        artifact_document.get("receipt_sha256") == detail.get("receipt_sha256"),
        f"{job_id}: result artifact receipt differs",
    )
    for field in ["status", "exit_code", "stdout", "stderr", "truncated", "violations"]:
        require(
            artifact_document.get(field) == result.get(field),
            f"{job_id}: result artifact {field} differs from the API result",
        )
    verified = offline_verifier.verify(
        envelope,
        artifact,
        execution_id=job_id,
        tenant=tenant,
        subject_name=f"coop://jobs/{job_id}/result",
        media_type=RESULT_ARTIFACT_MEDIA_TYPE,
        outcome=cast(str, result.get("status")),
        expected_key_id=key_id,
    )
    status["verified_key_ids"] = verified["verified_key_ids"]
    return status


def verify_canary(
    api: Api,
    language: str,
    minimum_isolation: str,
    tenant: str,
    attestation_capabilities: Mapping[str, Any],
    offline_verifier: OfflineVerifier,
) -> str:
    code, expected = CANARIES[language]
    submitted = api.request(
        "POST",
        "/v1/jobs",
        {
            "language": language,
            "code": code,
            "requirements": {"minimum_isolation": minimum_isolation},
            "limits": {
                "wall_seconds": 10,
                "mem_mb": 128,
                "allow_network": False,
            },
        },
    )
    job_id = submitted.get("job_id")
    require(
        isinstance(job_id, str) and job_id, f"{language}: submission returned no job ID"
    )
    result = api.request(
        "GET",
        f"/v1/jobs/{urllib.parse.quote(job_id, safe='')}/result?wait_seconds=60",
        timeout=70,
    )
    require(result.get("status") == "succeeded", f"{language}: canary did not succeed")
    require(result.get("exit_code") == 0, f"{language}: canary exit code is not zero")
    require(result.get("stdout") == expected, f"{language}: unexpected stdout")
    require(result.get("stderr") == "", f"{language}: unexpected stderr")
    require(result.get("truncated") is False, f"{language}: result was truncated")
    detail = api.request("GET", f"/v1/jobs/{urllib.parse.quote(job_id, safe='')}")
    policy = detail.get("execution_policy")
    require(isinstance(policy, dict), f"{language}: execution policy is missing")
    require(
        policy.get("bootstrap_ready") is True, f"{language}: bootstrap was not ready"
    )
    observed_policy = {"backend": policy.get("sandbox"), **policy}
    require_execution(
        observed_policy,
        f"{language} policy",
        minimum_isolation,
        terminal_evidence=True,
    )
    requested = detail.get("requested_spec")
    require(isinstance(requested, dict), f"{language}: requested spec is missing")
    require(
        requested.get("requirements") == {"minimum_isolation": minimum_isolation},
        f"{language}: requested minimum isolation was not retained",
    )
    effective = detail.get("effective_spec")
    require(isinstance(effective, dict), f"{language}: effective spec is missing")
    require(
        effective.get("requirements") == {"minimum_isolation": minimum_isolation},
        f"{language}: effective minimum isolation differs",
    )
    effective_class = effective.get("isolation_class")
    require(
        isinstance(effective_class, str)
        and isolation_satisfies(effective_class, minimum_isolation),
        f"{language}: effective isolation class does not satisfy the minimum",
    )
    verify_receipt(detail, minimum_isolation)
    verify_attested_result(
        api,
        job_id,
        tenant,
        detail,
        result,
        attestation_capabilities,
        offline_verifier,
    )
    return job_id


def parse_languages(raw: str) -> List[str]:
    values = [value.strip().lower() for value in raw.split(",") if value.strip()]
    require(bool(values), "COOP_VERIFY_LANGUAGES must not be empty")
    unknown = set(values).difference(CANARIES)
    require(
        not unknown, f"unsupported canary language(s): {', '.join(sorted(unknown))}"
    )
    return values


def main() -> None:
    try:
        offline_verifier = OfflineVerifier.from_environment()
        api = Api(
            os.environ.get("COOP_VERIFY_BASE_URL", "http://127.0.0.1:7300"),
            os.environ.get("COOP_CLIENT_KEY", ""),
        )
        languages = parse_languages(
            os.environ.get("COOP_VERIFY_LANGUAGES", "python,node,bash")
        )
        minimum_isolation = parse_minimum_isolation(
            os.environ.get("COOP_VERIFY_MINIMUM_ISOLATION", DEFAULT_MINIMUM_ISOLATION)
        )
        identity = api.request("GET", "/v1/whoami")
        tenant = identity.get("tenant")
        require(isinstance(tenant, str) and bool(tenant), "authenticated tenant is missing")
        status = api.request("GET", "/v1/status")
        execution = status.get("execution")
        require(isinstance(execution, dict), "status execution posture is missing")
        observed_class = require_execution(execution, "status", minimum_isolation)
        require(status.get("storage_ready") is True, "storage is not ready")
        scheduler = status.get("scheduler")
        require(
            isinstance(scheduler, dict) and scheduler.get("shutting_down") is False,
            "scheduler is shutting down",
        )

        capabilities = api.request("GET", "/v1/capabilities")
        advertised = capabilities.get("languages")
        require(isinstance(advertised, list), "capability languages are missing")
        for language in languages:
            require(language in advertised, f"runtime is not advertised: {language}")
        capability_execution = capabilities.get("execution")
        require(
            isinstance(capability_execution, dict),
            "capability execution posture is missing",
        )
        capability_class = require_execution(
            capability_execution, "capabilities", minimum_isolation
        )
        require(
            capability_class == observed_class,
            "status and capability isolation classes differ",
        )
        features = capabilities.get("features")
        require(
            isinstance(features, dict) and features.get("receipts") is True,
            "receipt capability is unavailable",
        )
        require(
            features.get("signed_attestations") is True,
            "signed-attestation capability is unavailable",
        )
        attestation_capabilities = capabilities.get("attestations")
        require(
            isinstance(attestation_capabilities, dict)
            and attestation_capabilities.get("enabled") is True,
            "attestation signer is not enabled",
        )
        require(
            attestation_capabilities.get("algorithm") == "Ed25519",
            "unexpected attestation algorithm",
        )
        require(
            attestation_capabilities.get("envelope_format")
            == "DSSE/in-toto Statement v1",
            "unexpected attestation envelope format",
        )
        require(
            isinstance(attestation_capabilities.get("key_id"), str),
            "attestation key ID is missing",
        )

        jobs = {
            language: verify_canary(
                api,
                language,
                minimum_isolation,
                tenant,
                attestation_capabilities,
                offline_verifier,
            )
            for language in languages
        }
        print(
            json.dumps(
                {
                    "ok": True,
                    "version": status.get("version"),
                    "backend": execution.get("backend"),
                    "isolation_class": observed_class,
                    "minimum_isolation": minimum_isolation,
                    "tenant": tenant,
                    "attestation_key_id": attestation_capabilities.get("key_id"),
                    "languages": languages,
                    "canary_job_ids": jobs,
                },
                separators=(",", ":"),
            )
        )
    except VerificationError as exc:
        print(f"production verification failed: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc


if __name__ == "__main__":
    main()
