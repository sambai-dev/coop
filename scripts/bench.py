#!/usr/bin/env python3
import argparse
import concurrent.futures
import json
import statistics
import threading
import time
import urllib.error
import urllib.request

lock = threading.Lock()


def make_request(base, key, method, path, payload=None, timeout=75, retries=5):
    data = json.dumps(payload).encode() if payload is not None else None
    for attempt in range(retries + 1):
        req = urllib.request.Request(
            base.rstrip("/") + path,
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {key}",
                "Content-Type": "application/json",
                "User-Agent": "rookhold-bench/0.7",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return json.loads(resp.read().decode() or "null")
        except urllib.error.HTTPError as exc:
            if exc.code != 429 or attempt == retries:
                body = exc.read().decode(errors="replace")
                raise RuntimeError(f"{method} {path}: HTTP {exc.code}: {body}") from exc
            retry_after = exc.headers.get("Retry-After")
            try:
                delay = max(0.25, float(retry_after)) if retry_after else 0.0
            except ValueError:
                delay = 0.0
            if not delay:
                delay = min(8.0, 0.5 * (2**attempt))
            time.sleep(delay)

    raise AssertionError("unreachable")


def run_job(base, key, code, wait_seconds):
    t0 = time.perf_counter()
    resp = make_request(base, key, "POST", "/v1/jobs", {"language": "python", "code": code})
    job_id = resp["job_id"]
    # One long-poll replaces the old 20 ms status loop. Apart from producing
    # misleading server load, that loop exhausted the default tenant rate
    # limit in seconds. A 202 means the explicit wait budget expired.
    view = make_request(
        base,
        key,
        "GET",
        f"/v1/jobs/{job_id}/result?wait_seconds={wait_seconds}",
        timeout=wait_seconds + 15,
    )
    terminal = {"succeeded", "failed", "timed_out", "oom_killed", "cancelled", "error"}
    if view.get("status") not in terminal:
        raise RuntimeError(
            f"job {job_id} remained {view.get('status')!r} after a {wait_seconds}s result wait; "
            "increase --wait-seconds or reduce concurrency"
        )
    return time.perf_counter() - t0, view["status"]


def percentile(sorted_values, p):
    if not sorted_values:
        return float("nan")
    idx = min(len(sorted_values) - 1, max(0, round(p / 100 * (len(sorted_values) - 1))))
    return sorted_values[idx]


def main():
    parser = argparse.ArgumentParser(description="Benchmark Rookhold end-to-end job latency")
    parser.add_argument("--url", default="http://127.0.0.1:7300")
    parser.add_argument("--key", default="rookhold-dev-key")
    parser.add_argument("--jobs", type=int, default=50)
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument("--wait-seconds", type=int, default=60)
    args = parser.parse_args()

    if args.jobs < 1 or args.concurrency < 1:
        parser.error("--jobs and --concurrency must be positive")
    if not 1 <= args.wait_seconds <= 300:
        parser.error("--wait-seconds must be between 1 and 300")

    code = "print('bench')\n"

    print(f"warmup: 2 jobs")
    for _ in range(2):
        run_job(args.url, args.key, code, args.wait_seconds)

    print(f"running {args.jobs} jobs at concurrency {args.concurrency}...")
    latencies = []
    statuses = {}
    wall_start = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        futures = [
            pool.submit(run_job, args.url, args.key, code, args.wait_seconds)
            for _ in range(args.jobs)
        ]
        for fut in concurrent.futures.as_completed(futures):
            latency, status = fut.result()
            with lock:
                latencies.append(latency)
                statuses[status] = statuses.get(status, 0) + 1
    wall = time.perf_counter() - wall_start

    latencies.sort()
    ms = [v * 1000 for v in latencies]
    print()
    print("| metric | value |")
    print("|---|---|")
    print(f"| jobs | {len(ms)} |")
    print(f"| concurrency | {args.concurrency} |")
    print(f"| throughput | {len(ms) / wall:.1f} jobs/s |")
    print(f"| mean latency | {statistics.mean(ms):.0f} ms |")
    print(f"| p50 | {percentile(ms, 50):.0f} ms |")
    print(f"| p95 | {percentile(ms, 95):.0f} ms |")
    print(f"| p99 | {percentile(ms, 99):.0f} ms |")
    print(f"| max | {ms[-1]:.0f} ms |")
    print(f"| statuses | {statuses} |")


if __name__ == "__main__":
    main()
