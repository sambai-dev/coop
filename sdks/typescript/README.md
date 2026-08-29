# Coop TypeScript SDK

The typed client runs in modern browsers and Node.js 18+. It uses the
platform `fetch`, supports `AbortSignal` and per-request deadlines, and has no
runtime dependencies.

Browser use is same-origin by default. Coop intentionally does not emit
permissive CORS headers; a cross-origin frontend needs an explicitly
allowlisted reverse-proxy policy, and its bearer key will be available to that
frontend's JavaScript.

Install from a source checkout (available now):

```bash
git clone https://github.com/sambai-dev/coop.git
cd coop/sdks/typescript
npm ci
npm run build
npm pack
# Then, from your application:
npm install /path/to/coop/sdks/typescript/coop-sdk-0.3.0.tgz
```

The npm package is not published yet. After it is published, install it with:

```bash
npm install coop-sdk
```

The exact v0.3.0 GitHub release package is
`coop-sdk-0.3.0.tgz`; follow the
[checksum, attestation, and installation commands](../../docs/sdks.md) rather
than using a moving release URL.

```ts
import { Coop } from "coop-sdk";

const coop = new Coop("http://127.0.0.1:7300", "tenant-api-key");
const job = await coop.submit("node", "console.log(6 * 7)", {
  limits: { wall_seconds: 10, mem_mb: 256 },
  requirements: { minimum_isolation: "linux-shared-kernel" },
});

for await (const event of coop.streamEvents(job.job_id)) {
  console.log(event.kind, event.data);
}

const result = await coop.result(job.job_id, 60_000);
console.log(result.status, result.stdout);
```

For an idempotent v0.4 submission, use `submitResult()`. It preserves the
ordinary response body under `job` and also exposes the `Location` and
`Idempotency-Replayed` response headers:

```ts
const accepted = await coop.submitResult("python", "print('once')", {
  idempotencyKey: "workflow-run-018f6f8d", // 1-128 visible ASCII bytes
  retryAmbiguous: true,
  requirements: { minimum_isolation: "gvisor-application-kernel" },
});
console.log(accepted.job.job_id, accepted.location, accepted.idempotency_replayed);
```

`retryAmbiguous` makes at most one automatic retry and reuses the same key. Use
it only with a server that enforces v0.4 idempotency semantics. An ambiguous
failure carries the stable key on `CoopError.idempotencyKey` so a caller can
persist it and reconcile the logical request later. `cancelResult()` returns
the typed v0.4 cancellation state; `cancel()` remains the body-discarding
compatibility wrapper.

`capabilities()` exposes the provider's `isolation_class`, per-job
`mem_mb_max`, and aggregate `concurrent_mem_mb_max`. Use
`isolationSatisfies(actual, minimum)` for the server-compatible branched
ordering: `wasm-capability` satisfies only that capability requirement and is
not silently interchangeable with a shared-kernel or VM class.

Every network method accepts an `AbortSignal` directly or through its options.
`CoopError` exposes `status`, `code`, `requestId`, `retryable`, and
`retryAfterMs`.

Streaming obtains a short-lived, one-use v0.2 ticket before opening a
WebSocket. On runtimes without `WebSocket`, `streamEvents()` uses the cursor
replay endpoint. A v0.1 query-key fallback remains available for compatibility
but is disabled by default because URLs leak into logs and history. Enable it
only for a trusted legacy server with `allowLegacyQueryKey: true`; structured
v0.2 errors never trigger that fallback.

`submit()` accepts the sparse `JobSpec` shape, while `get()` and `wait()`
return `JobDetail`. The requested `StoredJobSpec` is complete.
`EffectiveJobSpec` uses `EffectiveLimits`: each control can be `null` when it
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
`terminal_reason: "server_restarted"`.

The deprecated callback `stream(jobId, onEvent, key)` form is rejected so a
credential cannot silently enter a URL. For a trusted v0.1 server, use the
explicit `{ allowLegacyQueryKey: true, legacyApiKey: key }` options object.
