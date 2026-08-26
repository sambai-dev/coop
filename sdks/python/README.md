# Coop Python SDK

A typed synchronous client with no required dependencies. Copy `coop.py` into
your project or install the package. Install the optional stream extra for a
native WebSocket; otherwise `stream()` transparently uses cursor replay.

Install from a source checkout (available now):

```bash
git clone https://github.com/sambai-dev/coop.git
python -m pip install "./coop/sdks/python[stream]"
```

The PyPI package is not published yet. After it is published, install it with:

```bash
python -m pip install "coop-sdk[stream]"
```

The exact v0.2.0 GitHub release wheel is
`coop_sdk-0.2.0-py3-none-any.whl`; follow the
[checksum, attestation, and installation commands](../../docs/sdks.md) rather
than using a moving release URL.

```python
from coop import Coop, Limits

client = Coop("http://127.0.0.1:7300", "tenant-api-key", timeout=30)
job = client.submit(
    "python",
    "print(sum(range(10)))",
    limits=Limits(wall_seconds=10, mem_mb=256),
)

for event in client.stream(job["job_id"]):
    print(event["kind"], event.get("data"))

result = client.result(job["job_id"], timeout=60)
print(result["status"], result["stdout"])
```

The v0.2 client obtains a one-use stream ticket, so API keys do not appear in
WebSocket URLs. The legacy v0.1 query-key fallback is disabled by default
because URLs leak into logs and history. Enable it only for a trusted legacy
server with `allow_legacy_query_key=True`; structured v0.2 errors never trigger
that fallback.

`CoopError` exposes `status`, `code`, `request_id`, `retryable`, and
`retry_after`. Transport and invalid-response errors use the same error type.

`submit()` accepts the sparse `JobSpec` shape, while `get()` and `wait()`
return `JobDetail`. The requested `StoredJobSpec` is complete.
`EffectiveJobSpec` uses `EffectiveLimits`: each control can be `None` when it
was not enforced, and `LimitEnforcement` supplies the explicit booleans. The
whole effective spec and policy fields are nullable when a queued, migrated,
or restart-recovered row has no execution evidence. `Receipt`,
`EventChainReceipt`, `OutputEvidence`, `ExecutorOutputEvidence`,
`ResourceUsage`, and `HashedCoopEvent` are exported for typed
evidence-processing code. Receipt `output` covers canonical durable event
strings; `executor_output` separately describes raw executor telemetry.
Keep accepting `CoopEvent` when reading migrated v0.1 history because its hash
fields may be absent or null.
The receipt core and event chain are always present; execution-specific receipt
fields are optional because restart recovery creates a minimal receipt with
`terminal_reason="server_restarted"`.
