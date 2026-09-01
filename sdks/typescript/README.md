# Rookhold TypeScript SDK

The typed client runs in modern browsers and Node.js 18+. It uses the
platform `fetch`, supports `AbortSignal` and per-request deadlines, and has no
runtime dependencies.

Browser use is same-origin by default. Rookhold intentionally does not emit
permissive CORS headers; a cross-origin frontend needs an explicitly
allowlisted reverse-proxy policy, and its bearer key will be available to that
frontend's JavaScript.

Before v0.8.0 publishes, build and pack the checkout:

```bash
cd sdks/typescript
npm ci
npm pack
```

After publication, install from npm:

```bash
npm install rookhold
```

The common path is one call:

```ts
import { Rookhold } from "rookhold";

const result = await Rookhold.fromEnv().run({
  language: "python",
  code: "print(6 * 7)",
});
console.log(result.raiseForStatus().stdout);
```

The exact v0.8.0 GitHub release package is
`rookhold-0.8.0.tgz`; follow the
[checksum, attestation, and installation commands](../../docs/sdks.md) rather
than using a moving release URL.

```ts
import { Rookhold } from "rookhold";

const client = new Rookhold("http://127.0.0.1:7300", "tenant-api-key");
const job = await client.submit("node", "console.log(6 * 7)", {
  limits: { wall_seconds: 10, mem_mb: 256 },
  requirements: { minimum_isolation: "linux-shared-kernel" },
});

for await (const event of client.streamEvents(job.job_id)) {
  console.log(event.kind, event.data);
}

const result = await client.result(job.job_id, 60_000);
console.log(result.status, result.stdout);
```

For a terminal job whose `detail.attestation.available` is true, preserve the
exact DSSE envelope and signed result subject. In Node.js:

```ts
import { writeFile } from "node:fs/promises";

const detail = await client.get(job.job_id);
if (detail.attestation.available) {
  const envelope = await client.downloadAttestation(job.job_id);
  const subject = await client.downloadResultArtifact(job.job_id);
  await writeFile("job.dsse.json", envelope.content);
  await writeFile("job-result.json", subject.content);
}
```

The downloads remain authenticated to the tenant, set `redirect: "error"`,
retain binary order, and validate `X-Content-Sha256`. The returned
`contentType`, `contentLength`, and `sha256` describe those exact bytes; they do
not mean the DSSE signature was verified. Pin the operator's Ed25519 public key
through an authenticated out-of-band channel, then run:

```bash
rookhold-verify verify \
  --envelope job.dsse.json \
  --subject job-result.json \
  --public-key trusted-rookhold-attestation.pub.pem \
  --tenant "$EXPECTED_TENANT" \
  --subject-name "coop://jobs/$JOB_ID/result" \
  --media-type application/vnd.coop.execution-result.v1+json
```

The signed predicate and exact result both bind the authoritative tenant;
`detail.attestation.tenant` exposes the same expected claim once available.
Pass a workflow-expected tenant to the verifier rather than copying one from
untrusted downloaded JSON. Migrated v0.3 receipts may omit tenant.

`attestationPublicKey()` is typed discovery data, not a trust anchor. Exit zero
authenticates the envelope profile and exact subject; callers must separately
evaluate `outcome` and `event_chain_complete`.

For an idempotent v0.4 submission, use `submitResult()`. It preserves the
ordinary response body under `job` and also exposes the `Location` and
`Idempotency-Replayed` response headers:

```ts
const accepted = await client.submitResult("python", "print('once')", {
  idempotencyKey: "workflow-run-018f6f8d", // 1-128 visible ASCII bytes
  retryAmbiguous: true,
  requirements: { minimum_isolation: "gvisor-application-kernel" },
});
console.log(accepted.job.job_id, accepted.location, accepted.idempotency_replayed);
```

`retryAmbiguous` makes at most one automatic retry and reuses the same key. Use
it only with a server that enforces v0.4 idempotency semantics. An ambiguous
failure carries the stable key on `RookholdError.idempotencyKey` so a caller can
persist it and reconcile the logical request later. `cancelResult()` returns
the typed v0.4 cancellation state; `cancel()` remains the body-discarding
compatibility wrapper.

`capabilities()` exposes the provider's `isolation_class`, per-job
`mem_mb_max`, and aggregate `concurrent_mem_mb_max`. Use
`isolationSatisfies(actual, minimum)` for the server-compatible branched
ordering: `wasm-capability` satisfies only that capability requirement and is
not silently interchangeable with a shared-kernel or VM class.

Every network method accepts an `AbortSignal` directly or through its options.
`RookholdError` exposes `status`, `code`, `requestId`, `retryable`, and
`retryAfterMs`.

Streaming obtains a short-lived, one-use ticket before opening a
WebSocket. On runtimes without `WebSocket`, `streamEvents()` uses the cursor
replay endpoint. A v0.1 query-key fallback remains available for compatibility
but is disabled by default because URLs leak into logs and history. Enable it
only for a trusted legacy server with `allowLegacyQueryKey: true`; structured
Rookhold errors never trigger that fallback.

`submit()` accepts the sparse `JobSpec` shape, while `get()` and `wait()`
return `JobDetail`. The requested `StoredJobSpec` is complete.
`EffectiveJobSpec` uses `EffectiveLimits`: each control can be `null` when it
was not enforced, and `LimitEnforcement` supplies the explicit booleans. The
whole effective spec and policy fields are nullable when a queued, migrated,
or restart-recovered row has no execution evidence. `Receipt`,
`EventChainReceipt`, `OutputEvidence`, `ExecutorOutputEvidence`,
`ResourceUsage`, and `HashedRookholdEvent` are exported for typed
evidence-processing code. Receipt `output` covers canonical durable event
strings; `executor_output` separately describes raw executor telemetry.
Keep accepting `RookholdEvent` when reading migrated v0.1 history because its hash
fields may be absent or null.
The receipt core and event chain are always present; execution-specific receipt
fields are optional because restart recovery creates a minimal receipt with
`terminal_reason: "server_restarted"`.

The deprecated callback `stream(jobId, onEvent, key)` form is rejected so a
credential cannot silently enter a URL. For a trusted v0.1 server, use the
explicit `{ allowLegacyQueryKey: true, legacyApiKey: key }` options object.

Existing imports can migrate in stages: `rookhold/coop` exports `Coop`,
`CoopError`, and the legacy event type names as aliases of the Rookhold API.
