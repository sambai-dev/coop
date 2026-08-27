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
                "COOP_VERIFY_BASE_URL must not contain credentials, a query, or fragment"
            )
        if parts.scheme == "http" and parts.hostname not in {"127.0.0.1", "localhost"}:
            raise VerificationError(
                "plain HTTP verification is allowed only on loopback; use HTTPS remotely"
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


def require_isolated_execution(document: Mapping[str, Any], label: str) -> None:
    require(
        document.get("backend") == "namespaces+cgroups-v2+private-rootfs",
        f"{label}: unexpected backend",
    )
    for field in ["isolated", "private_rootfs", "dedicated_bootstrap", "seccomp"]:
        require(document.get(field) is True, f"{label}: {field} is not true")
    require(
        document.get("networking") == "disabled", f"{label}: networking is not disabled"
    )
    enforcement = document.get("limit_enforcement")
    require(isinstance(enforcement, dict), f"{label}: limit enforcement is missing")
    for field in ["wall_seconds", "cpu_seconds", "mem_mb", "max_pids", "max_file_mb"]:
        require(enforcement.get(field) is True, f"{label}: {field} is not enforced")


def verify_receipt(detail: Mapping[str, Any]) -> None:
    receipt = detail.get("receipt")
    require(isinstance(receipt, dict), "terminal detail has no receipt")
    require(
        receipt.get("bootstrap_ready") is True,
        "receipt did not observe bootstrap readiness",
    )
    require_isolated_execution(receipt, "receipt")
    require(receipt.get("network_allowed") is False, "receipt allowed networking")
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
        require(
            isinstance(output.get(field), str) and len(output[field]) == 64,
            f"invalid {field}",
        )
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


def verify_canary(api: Api, language: str) -> str:
    code, expected = CANARIES[language]
    submitted = api.request(
        "POST",
        "/v1/jobs",
        {
            "language": language,
            "code": code,
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
        policy.get("sandbox") == "namespaces+cgroups-v2+private-rootfs",
        f"{language}: unexpected observed sandbox",
    )
    require(
        policy.get("bootstrap_ready") is True, f"{language}: bootstrap was not ready"
    )
    require_isolated_execution(
        {"backend": policy.get("sandbox"), **policy}, f"{language} policy"
    )
    require(
        policy.get("network_allowed") is False, f"{language}: networking was allowed"
    )
    verify_receipt(detail)
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
        status = api.request("GET", "/v1/status")
        execution = status.get("execution")
        require(isinstance(execution, dict), "status execution posture is missing")
        require_isolated_execution(execution, "status")
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
        require_isolated_execution(capability_execution, "capabilities")
        features = capabilities.get("features")
        require(
            isinstance(features, dict) and features.get("receipts") is True,
            "receipt capability is unavailable",
        )

        jobs = {language: verify_canary(api, language) for language in languages}
        print(
            json.dumps(
                {
                    "ok": True,
                    "version": status.get("version"),
                    "backend": execution.get("backend"),
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
