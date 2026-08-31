from __future__ import annotations

import runpy
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECK = runpy.run_path(str(ROOT / "scripts" / "check-pr-description.py"))


def valid_body() -> str:
    return """## Declared scope
Repair one independently useful parser behavior without claiming the sibling command.

## Root invariant
Every accepted candidate is classified once and rejected candidates never fall through.

## Reproduction and RED evidence
RED: the focused test failed on the unchanged source because the valid label was skipped.

## Changes
Repair the classifier predicate and keep the parser contract otherwise unchanged.

## Adversarial coverage
- [x] null-like and annotated values
- [x] leading spacer cells and classifier fallthrough

## Validation on final head
- Head: 0123456789abcdef
- Checks: focused regression, affected suite, lint, and required CI are green.

## Non-goals and remaining work
The separate all-target command remains out of scope and is recorded independently.

## Safety and compatibility
No wire-format change; the accepted input set only gains previously valid cases.

## Completion state
- Implementation: complete for the declared scope
- Validation: complete on the exact head above
- Review: pending maintainer approval
- Integration: pending merge through the protected branch
"""


class PullRequestDescriptionTests(unittest.TestCase):
    def test_complete_contract_passes(self) -> None:
        self.assertEqual(CHECK["validate_body"](valid_body()), [])

    def test_missing_scope_and_placeholder_sections_fail(self) -> None:
        body = valid_body().replace("## Declared scope", "## Context").replace(
            "RED: the focused test failed on the unchanged source because the valid label was skipped.",
            "<!-- add evidence -->",
        )
        errors = CHECK["validate_body"](body)
        self.assertIn("missing section: Declared scope", errors)
        self.assertIn(
            "section has no concrete evidence: Reproduction and RED evidence", errors
        )

    def test_adversarial_and_completion_fields_cannot_remain_template_defaults(self) -> None:
        body = valid_body().replace("- [x] null-like and annotated values", "- [ ] null-like values")
        body = body.replace("- [x] leading spacer cells and classifier fallthrough", "")
        body = body.replace(
            "- Validation: complete on the exact head above",
            "- Validation: choose complete or pending",
        )
        errors = CHECK["validate_body"](body)
        self.assertTrue(any("Adversarial coverage" in error for error in errors))
        self.assertIn("Completion state must declare validation status", errors)

    def test_final_head_requires_sha_and_concrete_checks(self) -> None:
        body = valid_body().replace("- Head: 0123456789abcdef", "- Head: latest")
        body = body.replace(
            "- Checks: focused regression, affected suite, lint, and required CI are green.",
            "- Checks: ok",
        )
        errors = CHECK["validate_body"](body)
        self.assertIn("Validation on final head must include 'Head: <commit SHA>'", errors)
        self.assertIn(
            "Validation on final head must include concrete 'Checks:'", errors
        )


if __name__ == "__main__":
    unittest.main()
