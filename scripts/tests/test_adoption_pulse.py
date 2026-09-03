from __future__ import annotations

import runpy
import unittest
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODULE = runpy.run_path(str(ROOT / "scripts" / "adoption-pulse.py"))


class AdoptionPulseTests(unittest.TestCase):
    def test_external_identity_filter_excludes_owner_and_bots(self) -> None:
        is_external = MODULE["is_external"]
        self.assertFalse(is_external("sambai-dev", "sambai-dev"))
        self.assertFalse(is_external("dependabot[bot]", "sambai-dev"))
        self.assertFalse(is_external("app/dependabot", "sambai-dev"))
        self.assertTrue(is_external("outside-user", "sambai-dev"))

    def test_report_prioritizes_first_runs_and_qualifies_clone_traffic(self) -> None:
        report = MODULE["build_report"](
            {
                "repository": "sambai-dev/rookhold",
                "stars": 11,
                "forks": 0,
                "watchers": 0,
                "unique_views": 19,
                "unique_cloners": 210,
                "first_run_feedback": 0,
                "external_issue_authors": 0,
                "external_pr_authors": 0,
                "external_contributors": 0,
                "latest_tag": "v0.8.0",
                "latest_downloads": 4,
            },
            datetime(2026, 9, 3, tzinfo=timezone.utc),
        )
        self.assertIn("First-run feedback reports | 0 | 5 | 5 to go", report)
        self.assertIn("CI matrix creates many automated checkouts", report)
        self.assertIn("Ask five outside developers", report)
        self.assertNotIn("210 | 210", report)

    def test_report_does_not_turn_unavailable_traffic_into_zero(self) -> None:
        report = MODULE["build_report"](
            {
                "repository": "sambai-dev/rookhold",
                "stars": 11,
                "forks": 0,
                "watchers": 0,
                "unique_views": None,
                "unique_cloners": None,
                "first_run_feedback": 0,
                "external_issue_authors": 0,
                "external_pr_authors": 0,
                "external_contributors": 0,
                "latest_tag": "v0.8.0",
                "latest_downloads": 4,
            },
            datetime(2026, 9, 3, tzinfo=timezone.utc),
        )
        self.assertIn("Unavailable to workflow token", report)
        self.assertNotIn("Unique repository visitors, rolling 14 days | 0", report)


if __name__ == "__main__":
    unittest.main()
