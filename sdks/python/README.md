# Coop Python SDK

A typed synchronous client and stdio MCP adapter with no required dependencies.
Install the optional stream extra for a native WebSocket; otherwise `stream()`
transparently uses cursor replay.

Install from a source checkout (available now):

```bash
git clone https://github.com/sambai-dev/coop.git
python -m pip install "./coop/sdks/python[stream]"
```

The PyPI package is not published yet. After it is published, install it with:

```bash
python -m pip install "coop-sdk[stream]"
```

The exact v0.4.0 GitHub release wheel is
`coop_sdk-0.4.0-py3-none-any.whl`; follow the
[checksum, attestation, and installation commands](../../docs/sdks.md) rather
than using a moving release URL.

Installation also provides the `coop-mcp` command. It exposes run, result,
evidence, and cancellation tools to MCP hosts while keeping the Coop URL and
key outside model-visible arguments:

```bash
export COOP_BASE_URL=http://127.0.0.1:7300
export COOP_API_KEY=coop-dev-key
coop-mcp
```

Normally an MCP host launches the command. Use the [Hermes, OpenClaw, and
generic host templates](../../integrations/README.md), and set
`COOP_MCP_MINIMUM_ISOLATION=gvisor-application-kernel` in the guarded
production deployment. The legacy `COOP_MCP_REQUIRE_ISOLATION=true` maps only
to `linux-shared-kernel`; prefer the exact class name. Coop
checks the minimum atomically at submission and the adapter validates the
terminal observed `isolation_class` without assuming namespace-specific
rootfs or seccomp details.

For `coop_run_code`, the adapter uses one UUID across the initial HTTP submit
and one ambiguous retry. When both acknowledgements are lost, it retains that
key for ten minutes under a tenant-, policy-, and normalized-job fingerprint,
then reuses it for the next identical call. The process-local table is capped
at 1,024 active or unresolved operations and fails closed at capacity. A valid
acknowledged job ID clears the exact entry; changed submissions and later
intentional identical runs receive fresh keys. Do not rely on this bounded
window across adapter restarts or configure unbounded host retries.

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

For a terminal job with `detail["attestation"]["available"] == True`, download
the two immutable files without JSON decoding or re-encoding:

```python
from pathlib import Path

detail = client.get(job["job_id"])
if detail["attestation"]["available"]:
    envelope = client.download_attestation(job["job_id"])
    subject = client.download_result_artifact(job["job_id"])
    Path("job.dsse.json").write_bytes(envelope["content"])
    Path("job-result.json").write_bytes(subject["content"])
```

Both download methods require the tenant bearer credential, refuse
cross-origin redirects, preserve the exact bytes, and validate the response
`X-Content-Sha256`. That is transport-integrity checking, not signature
verification. Obtain and pin the operator's Ed25519 public key through an
authenticated out-of-band channel, then verify offline:

```bash
coop-verify verify \
  --envelope job.dsse.json \
  --subject job-result.json \
  --public-key trusted-coop-attestation.pub.pem \
  --subject-name "coop://jobs/$JOB_ID/result" \
  --media-type application/vnd.coop.execution-result.v1+json
```

`attestation_public_key()` is typed discovery data and includes an explicit
trust notice; fetching a key from the same server as the evidence does not make
it a trust anchor. Exit zero authenticates the attestation profile and subject
bytes, not a successful job outcome. Inspect the verifier's `outcome` and
`event_chain_complete` fields under application policy.

Pass `requirements={"minimum_isolation": "linux-shared-kernel"}` to enforce an
execution boundary at admission. `submit_result()` additively exposes the
response `Location` and `Idempotency-Replayed` metadata. Idempotency keys are
1–128 visible ASCII bytes; ambiguous retries remain opt-in and reuse one key.

The v0.2 client obtains a one-use stream ticket, so API keys do not appear in
WebSocket URLs. The legacy v0.1 query-key fallback is disabled by default
because URLs leak into logs and history. Enable it only for a trusted legacy
server with `allow_legacy_query_key=True`; structured v0.2 errors never trigger
that fallback.

`CoopError` exposes `status`, `code`, `request_id`, `retryable`, `retry_after`,
and the submission `idempotency_key` when relevant. Transport and
invalid-response errors use the same error type.

`submit()` accepts the sparse `JobSpec` shape, while `get()` and `wait()`
return `JobDetail`. The requested `StoredJobSpec` is complete.
`EffectiveJobSpec` uses `EffectiveLimits`: each control can be `None` when it
was not enforced, `isolation_class` is runtime-observed evidence, and
`LimitEnforcement` supplies the explicit booleans. The
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
