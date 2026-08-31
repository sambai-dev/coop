#!/usr/bin/env python3
"""Render the deterministic CLI demo HTML to WebM for ffmpeg conversion."""

from __future__ import annotations

import argparse
from pathlib import Path

from playwright.sync_api import sync_playwright


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--html", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    html = args.html.resolve()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        context = browser.new_context(
            viewport={"width": 960, "height": 540},
            record_video_dir=str(output.parent),
            record_video_size={"width": 960, "height": 540},
            reduced_motion="no-preference",
        )
        page = context.new_page()
        page.goto(html.as_uri(), wait_until="load")
        page.wait_for_function("window.__ready === true")
        page.wait_for_timeout(8_600)
        video = page.video
        page.close()
        context.close()
        if video is None:
            raise RuntimeError("Playwright did not create a video")
        video.save_as(str(output))
        browser.close()


if __name__ == "__main__":
    main()
