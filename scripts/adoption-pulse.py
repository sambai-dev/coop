#!/usr/bin/env python3
"""Build a conservative public adoption pulse from GitHub repository signals."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def is_external(login: object, owner: str) -> bool:
    if not isinstance(login, str) or not login:
        return False
    folded = login.casefold()
    return folded != owner.casefold() and not folded.endswith("[bot]") and folded != "app/dependabot"


class GitHub:
    def get(self, path: str) -> Any:
        result = subprocess.run(
            ["gh", "api", path, "-H", "X-GitHub-Api-Version: 2022-11-28"],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        require(result.returncode == 0, f"GitHub API request failed for {path}")
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise ValueError(f"GitHub API returned malformed JSON for {path}") from error

    def all(self, path: str) -> list[dict[str, Any]]:
        separator = "&" if "?" in path else "?"
        rows: list[dict[str, Any]] = []
        for page in range(1, 11):
            value = self.get(f"{path}{separator}per_page=100&page={page}")
            require(isinstance(value, list), f"GitHub API returned a non-list for {path}")
            require(all(isinstance(row, dict) for row in value), f"GitHub API returned malformed rows for {path}")
            rows.extend(value)
            if len(value) < 100:
                return rows
        raise ValueError(f"GitHub API pagination exceeded 1,000 rows for {path}")


def label_names(issue: dict[str, Any]) -> set[str]:
    labels = issue.get("labels")
    if not isinstance(labels, list):
        return set()
    return {
        label["name"]
        for label in labels
        if isinstance(label, dict) and isinstance(label.get("name"), str)
    }


def build_snapshot(client: GitHub, repository: str) -> dict[str, Any]:
    require(repository.count("/") == 1, "repository must use owner/name")
    owner, name = repository.split("/", 1)
    encoded = f"{quote(owner, safe='')}/{quote(name, safe='')}"
    repo = client.get(f"/repos/{encoded}")
    try:
        views = client.get(f"/repos/{encoded}/traffic/views")
        clones = client.get(f"/repos/{encoded}/traffic/clones")
    except ValueError:
        views = {}
        clones = {}
    issues = [
        row
        for row in client.all(f"/repos/{encoded}/issues?state=all")
        if "pull_request" not in row
    ]
    pulls = client.all(f"/repos/{encoded}/pulls?state=all")
    contributors = client.all(f"/repos/{encoded}/contributors?anon=1")
    releases = client.all(f"/repos/{encoded}/releases")

    external_issue_authors = {
        row.get("user", {}).get("login")
        for row in issues
        if isinstance(row.get("user"), dict)
        and is_external(row["user"].get("login"), owner)
    }
    external_pr_authors = {
        row.get("user", {}).get("login")
        for row in pulls
        if isinstance(row.get("user"), dict)
        and is_external(row["user"].get("login"), owner)
    }
    external_contributors = {
        row.get("login")
        for row in contributors
        if is_external(row.get("login"), owner)
    }
    feedback = [row for row in issues if "first-run-feedback" in label_names(row)]
    latest = next(
        (release for release in releases if not release.get("draft") and not release.get("prerelease")),
        None,
    )
    latest_downloads = 0
    latest_tag = "none"
    if isinstance(latest, dict):
        latest_tag = str(latest.get("tag_name") or "unknown")
        assets = latest.get("assets")
        if isinstance(assets, list):
            latest_downloads = sum(
                int(asset.get("download_count", 0))
                for asset in assets
                if isinstance(asset, dict)
            )
    return {
        "repository": repository,
        "stars": int(repo.get("stargazers_count", 0)),
        "forks": int(repo.get("forks_count", 0)),
        "watchers": int(repo.get("subscribers_count", 0)),
        "unique_views": views.get("uniques") if isinstance(views.get("uniques"), int) else None,
        "unique_cloners": clones.get("uniques") if isinstance(clones.get("uniques"), int) else None,
        "first_run_feedback": len(feedback),
        "external_issue_authors": len(external_issue_authors),
        "external_pr_authors": len(external_pr_authors),
        "external_contributors": len(external_contributors | external_pr_authors),
        "latest_tag": latest_tag,
        "latest_downloads": latest_downloads,
    }


def progress(value: int, target: int) -> str:
    return "met" if value >= target else f"{target - value} to go"


def display_optional(value: object) -> str:
    return str(value) if isinstance(value, int) else "Unavailable to workflow token"


def build_report(snapshot: dict[str, Any], captured_at: datetime) -> str:
    first_runs = int(snapshot["first_run_feedback"])
    forks = int(snapshot["forks"])
    contributors = int(snapshot["external_contributors"])
    next_action = (
        "Ask five outside developers to complete the two-minute quickstart and submit the first-run form."
        if first_runs < 5
        else "Turn the most repeated first-run friction into one bounded improvement or starter contribution."
    )
    return f"""<!-- rookhold-adoption-pulse -->
# Adoption pulse

Updated {captured_at.astimezone(timezone.utc).strftime('%Y-%m-%d %H:%M UTC')} from GitHub's repository APIs.

## Outcomes

| Signal | Current | Initial target | Status |
|---|---:|---:|---|
| First-run feedback reports | {first_runs} | 5 | {progress(first_runs, 5)} |
| Public forks | {forks} | 1 | {progress(forks, 1)} |
| External contributors | {contributors} | 1 | {progress(contributors, 1)} |

## Context

| Signal | Current |
|---|---:|
| Stars | {int(snapshot['stars'])} |
| Unique repository visitors, rolling 14 days | {display_optional(snapshot['unique_views'])} |
| {snapshot['latest_tag']} release asset downloads | {int(snapshot['latest_downloads'])} |
| External issue authors | {int(snapshot['external_issue_authors'])} |
| External pull-request authors | {int(snapshot['external_pr_authors'])} |

## Next action

{next_action}

## Interpretation guardrails

- Stars, views, and downloads are attention signals, not proof of a successful run.
- Unique cloners ({display_optional(snapshot['unique_cloners'])}) are not an adoption KPI because the CI matrix creates many automated checkouts.
- Rookhold sends no hidden product telemetry; this pulse uses GitHub aggregates and opt-in public feedback.
"""


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--repository", required=True)
    root.add_argument("--output", type=Path, required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        snapshot = build_snapshot(GitHub(), args.repository)
        args.output.write_text(
            build_report(snapshot, datetime.now(timezone.utc)),
            encoding="utf-8",
        )
    except (OSError, TypeError, ValueError) as error:
        print(f"adoption pulse failed: {error}", file=sys.stderr)
        return 1
    print(f"wrote conservative adoption pulse to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
