#!/usr/bin/env python3
"""Verify that the documented zero-configuration demo reports no containment."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def verify(payload: dict[str, Any], *, require_receipt_file: bool = True) -> None:
    result = payload.get("result")
    receipt = payload.get("receipt")
    require(isinstance(result, dict), "local demo JSON omits result")
    require(isinstance(receipt, dict), "local demo JSON omits receipt")
    require(result.get("status") == "succeeded", "local demo did not succeed")
    require(result.get("stdout") == "42", "local demo stdout differs from the documentation")
    require(receipt.get("networking") == "host", "local demo must report host networking")
    require(receipt.get("isolation_class") == "none", "local demo must report isolation none")
    receipt_path = payload.get("receipt_path")
    require(isinstance(receipt_path, str), "local demo did not save a receipt")
    require(Path(receipt_path).name == "receipt.json", "local demo receipt path is unexpected")
    if require_receipt_file:
        require(Path(receipt_path).is_file(), "local demo receipt file does not exist")


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
