# Observability contract

Rookhold emits privacy-bounded structured logs and process-local metrics. Neither
surface is an evidence ledger: job events, receipts, exact result artifacts,
and signed envelopes remain the durable source of truth.

## Request and trace correlation

Every HTTP request receives a server-generated UUIDv7 `x-request-id`. The same
identifier appears in structured request logs and in an error envelope. A
caller-supplied `x-request-id` is never trusted or reflected.

Rookhold validates W3C `traceparent` and `tracestate`, then starts a fresh local
trace. A valid remote trace/span pair is retained only as a link; it is not the
local parent and its sampling flag cannot force local recording. Invalid input
is reduced to a closed rejection reason. Raw `tracestate`, `baggage`, query
strings, authorization values, stream tickets, source, stdin, and job output
are never copied into correlation state.

Accepted jobs retain only the fixed-size request ID, local trace/span IDs, and
validated upstream link in process memory. That state intentionally disappears
on restart. A future persistence migration can store those fixed-size members
beside an execution attempt and reconstruct an OpenTelemetry `Link`; it must
not persist raw trace headers or baggage. This is the narrow durable integration
hook—schema v4 does not persist trace headers or baggage.

## Logs

`ROOKHOLD_LOG_FORMAT=json` selects newline-delimited JSON. Production mode defaults
to JSON; development defaults to compact text. `ROOKHOLD_LOG_FORMAT=compact` (or
`text`) overrides either default. `RUST_LOG` continues to control filtering.

JSON request and job records carry `request_id`, `trace_id`, `span_id`, local
`trace_flags`, matched route templates, and bounded upstream-link fields. Job
IDs may appear in logs for operator investigation, but never as metric labels.
Attestation retry warnings identify the job and bounded failure context but
never key material or result bytes. Rookhold does not send logs or traces over the network and has no Collector health
dependency. A future exporter must be bounded, optional, fail-open, and flushed
under an explicit shutdown deadline.

## Global metrics

Set a dedicated credential of at least 16 characters:

```dotenv
ROOKHOLD_METRICS_TOKEN=replace-with-an-operator-only-secret
```

This enables `GET /metrics`. The token is accepted only by that endpoint; tenant
API keys are rejected. Keep the endpoint on the operator network and scrape it
with `Authorization: Bearer $ROOKHOLD_METRICS_TOKEN`. If the setting is absent, the
endpoint returns `404` and tenant/job service is unaffected.

The endpoint negotiates OpenMetrics 1.0 when the `Accept` header permits it and
otherwise returns Prometheus text 0.0.4. It uses cached atomics and bounded
maps, performs no SQLite operation during a scrape, emits `Cache-Control:
no-store`, and never labels by tenant, job, request, trace, client address, raw
path, or error text.

Families cover:

- HTTP count, active work, and duration;
- submitted/completed jobs and actual executor activity;
- admission rejection and queue/capacity pressure;
- bounded storage-operation count, failures, and duration;
- output/evidence truncation, restart recovery, and retention;
- readiness components, process uptime, version, and revision.

`/v1/metrics` remains the authenticated tenant compatibility view. Its former
`coop_running_jobs` metric was not strictly execution-only, so it is now named
`coop_job_lifecycle_owners_current`. Use global `coop_executions_active` for
work currently inside an executor backend.

## Liveness and readiness

`/healthz` remains dependency-free. One background task probes the existing
SQLite read path and updates a bounded cache; `/readyz`, `/v1/status`, and
`/metrics` read that cache in O(1). A failure, a stale monitor, startup recovery,
or shutdown makes readiness fail closed. Probe responses are not cacheable.

This release's monitor proves read-path freshness only. It does not claim that
the filesystem has space for the next commit. Operators must continue to alert
on free bytes/inodes and run a dedicated authenticated, signed canary. A future
store-owned write probe can feed the same cache without adding database work to
public readiness requests or changing their response contract.
