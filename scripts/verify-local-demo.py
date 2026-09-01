#!/usr/bin/env python3
"""Verify that the documented zero-configuration demo reports no containment."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any, Optional
from uuid import UUID


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def verify(
    payload: dict[str, Any],
    *,
    expected_runs_root: Optional[Path] = None,
    require_receipt_file: bool = True,
) -> None:
    result = payload.get("result")
    receipt = payload.get("receipt")
    require(isinstance(result, dict), "local demo JSON omits result")
    require(isinstance(receipt, dict), "local demo JSON omits receipt")
    require(result.get("status") == "succeeded", "local demo did not succeed")
    require(result.get("stdout") == "42", "local demo stdout differs from the documentation")
    require(receipt.get("networking") == "host", "local demo must report host networking")
    require(receipt.get("isolation_class") == "none", "local demo must report isolation none")
    result_job_id = result.get("job_id")
    receipt_job_id = receipt.get("job_id")
    require(isinstance(result_job_id, str), "local demo result omits its job_id")
    require(receipt_job_id == result_job_id, "local demo result and receipt job IDs differ")
    try:
        parsed_job_id = UUID(result_job_id)
    except (ValueError, AttributeError) as error:
        raise AssertionError("local demo job_id is not a UUID") from error
    require(parsed_job_id.version == 7, "local demo job_id is not UUIDv7")

    receipt_path = payload.get("receipt_path")
    require(isinstance(receipt_path, str), "local demo did not save a receipt")
    require(receipt_path != "" and "\x00" not in receipt_path, "local demo receipt path is malformed")
    reported_path = Path(receipt_path)
    require(".." not in reported_path.parts, "local demo receipt path contains traversal")
    require(reported_path.name == "receipt.json", "local demo receipt path is unexpected")

    root = (expected_runs_root or (Path.cwd() / ".rookhold" / "runs")).resolve()
    candidate = reported_path if reported_path.is_absolute() else Path.cwd() / reported_path
    try:
        resolved = candidate.resolve(strict=require_receipt_file)
        relative = resolved.relative_to(root)
    except (OSError, ValueError) as error:
        raise AssertionError("local demo receipt path escapes .rookhold/runs") from error
    require(
        relative.parts == (result_job_id, "receipt.json"),
        "local demo receipt path is stale or belongs to another job",
    )
    if require_receipt_file:
        require(resolved.is_file(), "local demo receipt file does not exist")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result", type=Path, help="JSON emitted by `rookhold run --json`")
    args = parser.parse_args()
    payload = json.loads(args.result.read_text(encoding="utf-8"))
    require(isinstance(payload, dict), "local demo output must be a JSON object")
    verify(payload)
    print("local demo snapshot OK: status=succeeded network=host isolation=none")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
