# API and streaming

The OpenAPI document at `/openapi.json` is the machine-readable HTTP contract. This guide describes behavior that matters to an agent integration.

## Authentication

Send a bearer key on every `/v1/*` request except a WebSocket upgrade authenticated with the one-use stream ticket described below:

```http
Authorization: Bearer TENANT_KEY
```

The key maps to exactly one tenant. Job identifiers are not authorization tokens; access is always checked against the authenticated tenant. `/healthz`, `/readyz`, `/openapi.json`, and the dashboard shell are public by design, so do not put secrets in those responses.

Coop uses plain HTTP/1.1. Put it behind a TLS proxy for any connection that leaves the local host or a private encrypted network; the proxy may serve HTTP/2 or HTTP/3 to clients while using HTTP/1.1 on the private Coop hop.

## Submit

`POST /v1/jobs` accepts:

```json
{
  "language": "python",
  "code": "print('hello')",
  "stdin": "optional input\n",
  "requirements": {
    "minimum_isolation": "linux-shared-kernel"
  },
  "limits": {
    "wall_seconds": 15,
    "cpu_seconds": 10,
    "mem_mb": 256,
    "max_pids": 128,
    "max_file_mb": 16,
    "allow_network": false
  }
}
```

The namespace deployment supports `python`, `node`, and `bash` after its full
rootfs startup preflight succeeds. Development mode probes each configured host
runtime once at startup and advertises only the languages that pass the exact
sanitized-environment canary in `GET /v1/capabilities`. Submitting a known but
unavailable runtime returns `422 runtime_unavailable`; an unknown language
returns `400 unsupported_language`. Omitted limits receive defaults and all
client values are clamped to server ceilings. `allow_network: true` is rejected
rather than granting egress. Namespace execution reports
`networking: "disabled"`; a ready development subprocess truthfully reports
its retained host networking as `networking: "host"`.

`requirements.minimum_isolation` is checked atomically before a job is
persisted and again before execution. The process-provider order is
`none < linux-shared-kernel < gvisor-application-kernel < hardware-vm <
confidential-vm`; a stronger observed class satisfies a weaker minimum. The
`wasm-capability` class is a separate branch and satisfies only itself (or a
minimum of `none`). An unsatisfied minimum returns
`422 minimum_isolation_unsatisfied` without creating a job.

Source and stdin are each capped at 1 MiB after JSON decoding. The encoded request body is capped at 16 MiB so worst-case valid JSON escaping still fits without allowing unbounded buffering. Body reads have a 30-second deadline and global/per-tenant active-read caps; capacity failures are structured retryable `429`/`503` responses. Stored/emitted stdout and stderr are independently capped at 1 MiB and 10,000 records, with any single record split at 16 KiB. The executor continues draining after the storage cap so a noisy child cannot block supervision; the event history and receipt record truncation and observed byte counts.

A successful submission returns `201 Created` with the job ID, initial status,
relative stream/history URLs, `Location: /v1/jobs/{id}`, and an
`Idempotency-Replayed` response header. For safe reconciliation after an
ambiguous transport failure, send exactly one `Idempotency-Key` containing
1–128 visible ASCII bytes. Reusing that tenant-scoped key with the same
canonical job spec returns the original job and `Idempotency-Replayed: true`;
reusing it for a different spec returns `422 idempotency_key_reused`. Queue or
global lifetime-capacity saturation returns `503`; tenant lifetime saturation
returns `429`. Callers should honor `Retry-After` and use bounded exponential
backoff with jitter rather than retrying immediately.

## Status and cancellation

`GET /v1/jobs/{id}` returns the lifecycle projection, requested spec, effective
spec, execution policy, and (once terminal) receipt plus `receipt_sha256`.
`requested_spec` remains the complete clamped-input record. Compare a non-null
`effective_spec`—not the request—to understand what the selected backend
actually enforced. Its `EffectiveLimits` members are individually nullable:
the namespace backend sets all five resource controls after successful
bootstrap, while the development subprocess sets only `wall_seconds`; its
CPU, memory, process, and file values are `null`. `limit_enforcement` carries
the corresponding explicit booleans.

Executor posture is published only from the backend's observed workload-ready
boundary. A pre-ready workdir/helper/pivot/seccomp failure has
`bootstrap_ready: false`, does not claim isolation/rootfs/seccomp/bootstrap,
and has null effective controls and network posture. Queued, migrated, or
restart-recovered rows with no executor observation return
`effective_spec: null` and null execution-policy fields; the API does not
fabricate historical posture from current configuration. Terminal statuses
are:

- `succeeded`
- `failed`
- `timed_out`
- `oom_killed`
- `cancelled`
- `error`

`DELETE /v1/jobs/{id}` requests cancellation. It is idempotent and returns the
current job projection plus `cancellation_requested` and `already_terminal`;
repeating it for an already-terminal job remains `200`. Cancellation is
cooperative at the scheduler boundary and forceful at the executor boundary.
Once terminal, a job does not return to a running state.

## Waiting for a result

`GET /v1/jobs/{id}/result?wait_seconds=60` is the preferred agent-tool endpoint. It waits up to the requested server-side budget (maximum 300 seconds) and folds output into a single object:

```json
{
  "job_id": "…",
  "status": "succeeded",
  "exit_code": 0,
  "duration_ms": 42,
  "stdout": "hello",
  "stderr": "",
  "truncated": false,
  "violations": []
}
```

Do not poll job status in a short loop. It creates unnecessary SQLite load and consumes the same tenant rate budget as submissions. If a wait expires, retry with backoff or switch to the stream. A wait interrupted by server shutdown returns retryable `503 shutting_down` without constructing a potentially large partial result; retry the durable job after the service is ready again.

Detail, replay, and folded-result JSON use a capacity-one 64 KiB response pump under global/per-tenant lifetime admission. The response body owns that admission permit and the serialized buffer until EOF or connection teardown, so progressing clients receive the complete declared JSON even at low bandwidth. Every accepted connection has a 30-second write-progress deadline: if Hyper is trying to write response bytes and the socket accepts no bytes for that entire interval, the server closes the connection, drops the body, and reclaims its capacity. A positive socket write resets that deadline; the independent 10-minute absolute connection lifetime remains the outer bound.

The transport admits at most 256 connections, gives every HTTP/1 request head a total 30 seconds (including silent and partial-preface peers), and closes every connection after an absolute 10 minutes. These are compiled v0.2 safety invariants and are logged at startup. The connection permit and absolute deadline move into an upgraded WebSocket; idle reads do not arm the write-progress timer, but the absolute lifetime still requires cursor-based reconnect. The request-head timer ends before a handler, `/result` wait, response transfer, or WebSocket session begins. Graceful shutdown first lets those upgraded sockets close normally; if the bounded HTTP drain expires, dropping the server force-closes guarded socket I/O and reclaims the global permit.

A response cut short by a transport boundary does not satisfy its declared `Content-Length`; clients must reject it as incomplete JSON. Detail, result, and replay are idempotent reads and may be retried with backoff from durable state. For replay, advance `after` only after a complete page has decoded and been accepted, then resume from that page's `next_cursor`; never advance a cursor from a truncated response. Do not apply this rule blindly to `POST /v1/jobs`: a submission transport failure does not prove that the durable job was not accepted. Prefer replay pagination when consuming a large event history.

## Event history

`GET /v1/jobs/{id}/replay?after=SEQ&limit=N` returns an `{events,next_cursor}` page in sequence order. The cursor is exclusive. The endpoint name means *replay stored events*, not rerun the program. Consumers should preserve unknown event kinds and hash metadata for forward compatibility.

Common event kinds include lifecycle transitions, `stdout`, `stderr`, `violation`, `truncated`, and `finished`. Use the sequence field for ordering; do not infer order from client receive time.

### Receipts and hash chains

Every new v0.2 event carries `hash_version: 1`, `prev_hash`, and `event_hash`. The event digest covers a versioned domain separator, job ID, previous hash, sequence, timestamp, kind, and canonical JSON data. A migrated v0.1 event uses `hash_version: 0`; it is preserved but explicitly unverifiable.

The terminal receipt records code/stdin/policy hashes, requested and effective
limits, backend/seccomp/network posture, private-rootfs and dedicated-bootstrap
facts, lifecycle and outcome fields, resource observations when available,
output evidence, and the final event-chain metadata. `bootstrap_ready` is the
executor-observed readiness bit. When it is `false`, the isolation facts are
false, `network_allowed`/`networking` are null, `effective_limits` contains
null values, and every `limit_enforcement` flag is false. When executor
provenance is unknown, those execution-specific members are omitted entirely.
`output` is durable evidence: its byte counts and SHA-256 values cover retained
output event strings encoded as UTF-8 and joined with one LF between records
and no trailing LF
(`encoding: "utf8-event-lines-joined-by-lf-no-trailing-lf"`). `truncated`
says at least one retained stream exceeded its configured boundary.

When executor telemetry survived, the optional top-level `executor_output` keeps raw pre-persistence observations separately for each stream: `bytes_seen`, `bytes_offered_to_sink`, `records_offered_to_sink`, `raw_sha256`, and `executor_truncated`. Offered records may exceed what became durable if the bounded sink failed or saturated, so do not compare those raw digests to `output.*_sha256`. `resource_usage` contains wall/CPU/memory observations only.

If startup recovery finds a job that was running when the server stopped, it emits a minimal terminal receipt with `terminal_reason: "server_restarted"`, outcome/timing fields, the event-chain summary, and the receipt digest. Execution-only evidence such as effective limits, runtime posture, output hashes, and resource observations is unavailable for that interrupted run; clients must treat those members as optional rather than inventing zero values.

`event_chain.complete` means the terminal transaction saw no legacy rows and the count of v1 hashed rows equalled the stored event count. It is not an on-read cryptographic verification result; an auditor must still recompute every event link. `receipt_sha256` is SHA-256 over the canonical receipt JSON with that member removed. These values detect changes relative to the retained record, but they are not signatures or external attestations; a database administrator can rewrite values and recompute them.

## WebSocket stream

First send an authenticated `POST /v1/jobs/{id}/stream-ticket`. It returns a short-lived, one-use, job-bound ticket and stream URL. Open that URL with `ws://` (or `wss://` behind TLS). The server consumes the ticket before active-stream admission, sends persisted history first, then live events, and finishes after the terminal event. A `429`/`503` capacity rejection therefore requires minting a new ticket after `Retry-After`. Reconnect by minting a new ticket and continue from the last sequence; deduplicate by job ID and sequence number.

Browser WebSocket APIs cannot set an `Authorization` header. Stream tickets exist so the long-lived API key never needs to enter a WebSocket URL. The bundled clients disable their v0.1 API-key query fallback by default; enabling it requires an explicit opt-in and should be limited to a trusted legacy server because URLs leak into history, logs, and proxy telemetry. Structured v0.2 errors never trigger the fallback.

Clients must handle:

- connection establishment, live, reconnecting, and closed states
- a job becoming terminal between history replay and live subscription
- lag recovery and repeated frames
- output truncation
- credential rotation closing an existing connection

## Listing and metrics

`GET /v1/jobs?limit=N&cursor=CURSOR&status=STATUS&language=LANGUAGE` returns `{items,next_cursor}` for the authenticated tenant. Cursors are opaque; pass them through unchanged.

`GET /v1/capabilities` describes supported languages, the provider's
`isolation_class`, per-job ceilings, the aggregate `concurrent_mem_mb_max`, and
server features. `GET /v1/whoami` resolves the authenticated tenant and
principal/scopes. `GET /v1/metrics` is an operator surface; keep it private and
do not assume its values are tenant-billing counters. `/healthz` is liveness and
`/readyz` checks process/store readiness. Use authenticated `/v1/status` plus an
actual minimum-isolation canary to verify containment.

## Errors and rate limits

Errors use a stable JSON envelope and repeat the request ID in `X-Request-Id`:

```json
{
  "error": {
    "code": "rate_limited",
    "message": "request rate limit exceeded",
    "request_id": "…",
    "retryable": true
  }
}
```

Treat HTTP status codes and error codes as authoritative. In particular:

- `400` invalid request or unsupported language
- `401` missing/invalid key
- `404` missing job or foreign-tenant job
- `409` invalid lifecycle operation
- `422` unavailable runtime, unsatisfied minimum isolation, or an idempotency key reused for a different request
- `429` tenant rate budget or per-tenant body/result-wait/stream/response lifetime capacity exhausted
- `503` admission queue, bounded request/response lifetime capacity, shutdown, or worker service unavailable
- `507` filesystem free-space reserve prevents durable admission

Honor `Retry-After` when present. Retry reads automatically only when their
operation policy allows it. Retry an ambiguously acknowledged submission only
when it carried an `Idempotency-Key`, and reuse the exact key and job spec.
