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


def make_request(base, key, method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        base + path,
        data=data,
        method=method,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode() or "null")


def run_job(base, key, code):
    t0 = time.perf_counter()
    resp = make_request(base, key, "POST", "/v1/jobs", {"language": "python", "code": code})
    job_id = resp["job_id"]
    while True:
        view = make_request(base, key, "GET", f"/v1/jobs/{job_id}")
        if view["status"] in ("succeeded", "failed", "timed_out", "oom_killed", "error"):
            break
        time.sleep(0.02)
    return time.perf_counter() - t0, view["status"]


def percentile(sorted_values, p):
    if not sorted_values:
        return float("nan")
    idx = min(len(sorted_values) - 1, max(0, round(p / 100 * (len(sorted_values) - 1))))
    return sorted_values[idx]


def main():
    parser = argparse.ArgumentParser(description="Benchmark Coop end-to-end job latency")
    parser.add_argument("--url", default="http://127.0.0.1:7300")
    parser.add_argument("--key", default="coop-dev-key")
    parser.add_argument("--jobs", type=int, default=50)
    parser.add_argument("--concurrency", type=int, default=4)
    args = parser.parse_args()

    code = "print('bench')\n"

    print(f"warmup: 2 jobs")
    for _ in range(2):
        run_job(args.url, args.key, code)

    print(f"running {args.jobs} jobs at concurrency {args.concurrency}...")
    latencies = []
    statuses = {}
    wall_start = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        futures = [pool.submit(run_job, args.url, args.key, code) for _ in range(args.jobs)]
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
