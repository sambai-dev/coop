from __future__ import annotations

import runpy
import unittest
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODULE = runpy.run_path(str(ROOT / "scripts" / "adoption-pulse.py"))


class AdoptionPulseTests(unittest.TestCase):
    @staticmethod
    def snapshot(first_run_feedback: int = 0) -> dict[str, object]:
        return {
            "repository": "sambai-dev/rookhold",
            "stars": 11,
            "forks": 0,
            "watchers": 0,
            "unique_views": 19,
            "unique_cloners": 210,
            "first_run_feedback": first_run_feedback,
            "external_issue_authors": 0,
            "external_pr_authors": 0,
            "external_contributors": 0,
            "latest_tag": "v0.8.0",
            "latest_downloads": 4,
        }

    def test_external_identity_filter_excludes_owner_and_bots(self) -> None:
        is_external = MODULE["is_external"]
        self.assertFalse(is_external("sambai-dev", "sambai-dev"))
        self.assertFalse(is_external("dependabot[bot]", "sambai-dev"))
        self.assertFalse(is_external("app/dependabot", "sambai-dev"))
        self.assertTrue(is_external("outside-user", "sambai-dev"))

    def test_report_prioritizes_first_runs_and_qualifies_clone_traffic(self) -> None:
        report = MODULE["build_report"](
            self.snapshot(),
            datetime(2026, 9, 3, tzinfo=timezone.utc),
        )
        self.assertIn("First-run feedback reports | 0 | 5 | 5 to go", report)
        self.assertIn("CI matrix creates many automated checkouts", report)
        self.assertIn("Ask five outside developers", report)
        self.assertNotIn("210 | 210", report)

        for first_runs, expected in [
            (4, "Ask five outside developers"),
            (5, "Turn the most repeated first-run friction"),
        ]:
            with self.subTest(first_runs=first_runs):
                boundary_report = MODULE["build_report"](
                    self.snapshot(first_runs),
                    datetime(2026, 9, 3, tzinfo=timezone.utc),
                )
                self.assertIn(expected, boundary_report)

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

    def test_snapshot_handles_each_traffic_permission_failure(self) -> None:
        class FakeClient:
            def __init__(self, failed_path: str) -> None:
                self.failed_path = failed_path

            def get(self, path: str):
                if self.failed_path in path:
                    raise ValueError("permission denied")
                if path.endswith("/traffic/views"):
                    return {"uniques": 19}
                if path.endswith("/traffic/clones"):
                    return {"uniques": 210}
                return {"stargazers_count": 11, "forks_count": 0, "subscribers_count": 0}

            def all(self, path: str):
                return []

        for failed_path in ["/traffic/views", "/traffic/clones"]:
            with self.subTest(failed_path=failed_path):
                snapshot = MODULE["build_snapshot"](
                    FakeClient(failed_path), "sambai-dev/rookhold"
                )
                self.assertIsNone(snapshot["unique_views"])
                self.assertIsNone(snapshot["unique_cloners"])
                report = MODULE["build_report"](
                    snapshot,
                    datetime(2026, 9, 3, tzinfo=timezone.utc),
                )
                self.assertIn("Unavailable to workflow token", report)


if __name__ == "__main__":
    unittest.main()
