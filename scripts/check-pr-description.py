#!/usr/bin/env python3
"""Validate that a pull request records scope and exact-head evidence."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REQUIRED_SECTIONS = (
    "Declared scope",
    "Root invariant",
    "Reproduction and RED evidence",
    "Changes",
    "Adversarial coverage",
    "Validation on final head",
    "Non-goals and remaining work",
    "Safety and compatibility",
    "Completion state",
)

COMPLETION_LABELS = ("Implementation", "Validation", "Review", "Integration")


def sections(body: str) -> tuple[dict[str, str], list[str]]:
    parsed: dict[str, list[str]] = {}
    errors: list[str] = []
    current: str | None = None
    fenced = False
    for line in body.splitlines():
        if line.lstrip().startswith("```"):
            fenced = not fenced
        match = None if fenced else re.fullmatch(r"##\s+(.+?)\s*", line)
        if match:
            current = match.group(1)
            if current in parsed:
                errors.append(f"duplicate section: {current}")
            parsed.setdefault(current, [])
            continue
        if current is not None:
            parsed[current].append(line)
    return {name: "\n".join(lines) for name, lines in parsed.items()}, errors


def meaningful(value: str) -> str:
    without_comments = re.sub(r"<!--.*?-->", "", value, flags=re.DOTALL)
    return without_comments.strip()


def field_value(section: str, label: str) -> str | None:
    match = re.search(
        rf"^\s*-?\s*{re.escape(label)}\s*:\s*(.+?)\s*$",
        section,
        flags=re.IGNORECASE | re.MULTILINE,
    )
    return match.group(1).strip() if match else None


def validate_body(body: str) -> list[str]:
    parsed, errors = sections(body)
    for name in REQUIRED_SECTIONS:
        if name not in parsed:
            errors.append(f"missing section: {name}")
            continue
        value = meaningful(parsed[name])
        if len(value) < 16:
            errors.append(f"section has no concrete evidence: {name}")

    red = meaningful(parsed.get("Reproduction and RED evidence", ""))
    if red and not re.search(
        r"\b(red|fail(?:ed|s|ing)?|not applicable because)\b", red, re.IGNORECASE
    ):
        errors.append(
            "Reproduction and RED evidence must state the observed failure or "
            "explain 'Not applicable because ...'"
        )

    adversarial = meaningful(parsed.get("Adversarial coverage", ""))
    if adversarial and not (
        re.search(r"\[[xX]\]", adversarial)
        or re.search(r"not applicable because", adversarial, re.IGNORECASE)
    ):
        errors.append(
            "Adversarial coverage must check at least one considered case or "
            "explain 'Not applicable because ...'"
        )

    exact_head = meaningful(parsed.get("Validation on final head", ""))
    if exact_head:
        head = field_value(exact_head, "Head")
        checks = field_value(exact_head, "Checks")
        if head is None or not re.search(r"\b[0-9a-f]{7,40}\b", head, re.IGNORECASE):
            errors.append("Validation on final head must include 'Head: <commit SHA>'")
        if checks is None or len(checks) < 8:
            errors.append("Validation on final head must include concrete 'Checks:'")

    completion = meaningful(parsed.get("Completion state", ""))
    if completion:
        for label in COMPLETION_LABELS:
            value = field_value(completion, label)
            if value is None or len(value) < 3 or "choose" in value.casefold():
                errors.append(f"Completion state must declare {label.lower()} status")

    return sorted(set(errors))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--event", type=Path, required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        event = json.loads(args.event.read_text(encoding="utf-8"))
        pull_request = event.get("pull_request")
        if not isinstance(pull_request, dict):
            print("PR description check skipped: event is not a pull request")
            return 0
        body = pull_request.get("body")
        errors = validate_body(body if isinstance(body, str) else "")
    except (OSError, json.JSONDecodeError, TypeError) as error:
        print(f"PR description check failed: {error}", file=sys.stderr)
        return 1
    if errors:
        for error in errors:
            print(f"PR description check failed: {error}", file=sys.stderr)
        return 1
    print("PR description contract OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
