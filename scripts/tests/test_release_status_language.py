from __future__ import annotations

import runpy
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECKER = runpy.run_path(str(ROOT / "scripts" / "check-release-surface.py"))
CHECK_STATUS = CHECKER["check_release_status_language"]


class ReleaseStatusLanguageTests(unittest.TestCase):
    def test_candidate_requires_an_explicit_warning_on_primary_surfaces(self) -> None:
        CHECK_STATUS(
            "candidate",
            "0.8.0",
            {
                "README.md": "Rookhold v0.8.0 release candidate",
                "docs/index.md": "v0.8.0 release candidate",
            },
        )

        with self.assertRaisesRegex(AssertionError, "presents unpublished"):
            CHECK_STATUS(
                "candidate",
                "0.8.0",
                {
                    "README.md": "Rookhold v0.8.0",
                    "docs/index.md": "v0.8.0 release candidate",
                },
            )

    def test_current_rejects_every_prepublication_phrase(self) -> None:
        for phrase in [
            "release candidate",
            "after v0.8.0 publishes",
            "until then",
        ]:
            with self.subTest(phrase=phrase):
                with self.assertRaisesRegex(AssertionError, "pre-publication wording"):
                    CHECK_STATUS(
                        "current",
                        "0.8.0",
                        {"README.md": f"Install v0.8.0; {phrase}."},
                    )

    def test_current_accepts_stable_release_copy(self) -> None:
        CHECK_STATUS(
            "current",
            "0.8.0",
            {"README.md": "Install the current Rookhold v0.8.0 release."},
        )


if __name__ == "__main__":
    unittest.main()
