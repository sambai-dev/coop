from __future__ import annotations

import runpy
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECKER = runpy.run_path(str(ROOT / "scripts" / "check-release-surface.py"))
VALIDATE_FORM = CHECKER["validate_feedback_form_contract"]
VALIDATE_WORKFLOW = CHECKER["validate_adoption_workflow_contract"]


class AdoptionContractTests(unittest.TestCase):
    def test_current_form_and_workflow_pass_structural_validation(self) -> None:
        VALIDATE_FORM(
            (ROOT / ".github" / "ISSUE_TEMPLATE" / "first-run-feedback.yml").read_text(
                encoding="utf-8"
            )
        )
        VALIDATE_WORKFLOW(
            (ROOT / ".github" / "workflows" / "adoption-pulse.yml").read_text(
                encoding="utf-8"
            )
        )

    def test_malformed_form_and_misplaced_field_fail_closed(self) -> None:
        source = (
            ROOT / ".github" / "ISSUE_TEMPLATE" / "first-run-feedback.yml"
        ).read_text(encoding="utf-8")
        with self.assertRaisesRegex(AssertionError, "name differs|malformed"):
            VALIDATE_FORM(source.replace("name: First-run feedback", "name First-run feedback"))
        with self.assertRaisesRegex(AssertionError, "omits exact field"):
            VALIDATE_FORM(source.replace("    id: time_to_result", "      id: time_to_result"))

    def test_comment_only_or_additional_permissions_fail_closed(self) -> None:
        source = (ROOT / ".github" / "workflows" / "adoption-pulse.yml").read_text(
            encoding="utf-8"
        )
        comment_only = source.replace(
            "      issues: write # update the single public adoption pulse issue",
            "      # issues: write",
        )
        with self.assertRaisesRegex(AssertionError, "permissions"):
            VALIDATE_WORKFLOW(comment_only)
        additional = source.replace(
            "      issues: write # update the single public adoption pulse issue",
            "      issues: write\n      pull-requests: write",
        )
        with self.assertRaisesRegex(AssertionError, "permissions"):
            VALIDATE_WORKFLOW(additional)


if __name__ == "__main__":
    unittest.main()
