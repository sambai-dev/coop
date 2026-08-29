#!/usr/bin/env python3
"""Verify a running Coop deployment through its authenticated public API."""

from __future__ import annotations

import hashlib
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, List, Mapping, Optional, cast

MAX_JSON_BYTES = 4 * 1024 * 1024
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
            with urllib.request.urlopen(request, timeout=timeout) as response:
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


def verify_canary(api: Api, language: str, minimum_isolation: str) -> str:
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

        jobs = {
            language: verify_canary(api, language, minimum_isolation)
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
