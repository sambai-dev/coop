# Architecture

Coop is an execution gateway, scheduler, executor, and evidence store in one Rust process. It is optimized for short, stateless agent jobs on one node. SQLite is the persistence boundary; horizontal scheduling and persistent workspaces are outside the v0.2 contract.

```text
agent / SDK / dashboard
          │ HTTP + WebSocket
          ▼
┌──────────────────────────────┐
│ API boundary                │
│ auth · validation · limits  │
│ rate limiting · tenant scope│
└──────────────┬───────────────┘
               │ bounded queue
               ▼
┌──────────────────────────────┐
│ Scheduler                    │
│ workers · tenant admission   │
│ cancellation · finalization  │
└──────────────┬───────────────┘
               │ execution plan
               ▼
┌──────────────────────────────┐
│ Executor                     │
│ x86_64 rootfs · namespaces   │
│ cgroup v2 · rlimits · seccomp│
└──────────────┬───────────────┘
               │ ordered events
               ▼
┌──────────────────────────────┐
│ SQLite + live fan-out        │
│ job state · events · results │
└──────────────────────────────┘
```

## Crate boundaries

- `coop-types` owns JSON-facing job types, lifecycle enums, and compiled limit ceilings.
- `coop-store` owns the SQLite schema and persistence operations.
- `coop-exec` owns interpreter staging and execution backends.
- `coop-server` owns configuration, authentication, routing, admission, scheduling, WebSockets, OpenAPI, and the embedded dashboard.

The executor interface is intentionally narrower than an OCI runtime API: one
job enters, an ordered stream of output/violation events leaves, and one
terminal outcome plus executor-observed provenance returns. The provenance
ready bit is set only at the backend's actual workload-ready boundary. A future
gVisor/OCI backend should preserve that contract while producing its own
runtime provenance.

## Job state and events

The jobs table is the current-state projection. The events table is an ordered
operational history keyed by job and sequence number. The durable start
transaction stores an all-null, unobserved effective-policy snapshot because
it precedes executor readiness. The terminal transaction atomically replaces
that snapshot with executor-observed effective controls and binds its digest
into the finished event. If provenance is unavailable, the API leaves the
effective policy unknown rather than reconstructing it from configuration.
Clients must use job status as the authoritative current state and event
sequence for investigation.

“Replay” means retrieving the stored event history. It does not mean executing the code again or reproducing nondeterministic effects.

Output is bounded by both record and byte policies. The executor records truncation and continues supervising the process so timeout, cancellation, cgroup cleanup, and final state cannot be starved by output flood.

## Data and trust boundaries

Each API key maps to one tenant. Tenant ownership is checked on job detail, result, cancel, event history, and stream access. Foreign job IDs should be indistinguishable from missing IDs.

The server process, SQLite file, private rootfs, and host kernel remain trusted. Submitted source, stdin, interpreter behavior, and descendants are untrusted. A namespace job must not receive the server's database or outer host root as part of its filesystem. The in-tree containment backend is supported only on Linux x86_64 in v0.2; other platforms use the unisolated development executor.

See [security-boundary.md](security-boundary.md) for the complete trust-tier statement.

## Scale characteristics

v0.2 is a single-node design:

- a bounded in-memory admission queue
- a configured worker pool
- per-tenant concurrency controls
- SQLite WAL for concurrent readers and serialized writes
- an in-process live event fan-out, backed by persisted history for reconnects

Running multiple Coop servers against the same SQLite file is unsupported. A multi-node design requires a durable queue, distributed admission control, a network database, and an external stream bus.

## Non-goals in v0.2

- persistent or resumable sandboxes
- file upload/download or artifact stores
- arbitrary container images
- PTYs and interactive shells
- exposed ports and inbound networking
- snapshots, warm pools, or deterministic replay
- signed or independently anchored receipts, or remote attestation
- multi-node scheduling
