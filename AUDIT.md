# Coop v0.2 security review record

**Review date:** 2026-08-26

**Scope:** Rust workspace, execution boundary, API tenancy, scheduler, SQLite store, dashboard, SDKs, Docker/Compose, CI/release automation, and public documentation

**Status:** historical v0.2 release hardening record; not an external certification. v0.3 adds integration and setup surfaces without changing this execution boundary.

This file records the security properties reviewed for v0.2 and the evidence expected before release. It deliberately replaces v0.1 claims that overstated host-filesystem isolation, PID-namespace behavior, event-log immutability, and hostile-test coverage.

## Review outcome

The v0.2 design is an audit-first single-node execution gateway with a shared-kernel Linux x86_64 backend. The release boundary is narrower than “safely execute arbitrary hostile code anywhere”:

- namespace execution requires a private rootfs and never falls back to host `/`;
- PID-namespace setup includes a namespace PID 1/reaper rather than executing in the parent namespace;
- terminal cleanup targets the whole cgroup, not only the original process group;
- privilege drop, mount setup, cgroup controls, and seccomp are fail-closed in production;
- output has byte and record caps so a single unterminated line cannot allocate without bound;
- blank-tenant/weak-key production configurations and dangerous job-root paths are rejected;
- requested policy remains separate from executor-observed effective controls;
  development execution reports only wall-time enforcement, while namespace
  posture is activated only by the helper's workload-ready frame;
- effective policy, output digests, lifecycle, event-chain head, and receipt digest are persisted as execution evidence.

These changes materially improve the previous release, but they do not turn Linux namespaces or a privileged container into a VM boundary.

## Residual risks and accepted constraints

| ID | Area | Constraint | Required operator response |
|---|---|---|---|
| R-001 | Shared kernel | Namespace jobs share the VM kernel and interpreter attack surface | Dedicated VM; patch promptly; use a hardened external runtime for hostile multi-tenancy when integrated |
| R-002 | Container packaging | Compose uses `privileged: true` to manage namespaces/mounts/cgroups | Run only inside a dedicated VM; never on a mixed-trust Docker host |
| R-003 | Trusted server/store | The Coop process and SQLite administrator can rewrite state and recompute hashes | Restrict access; export evidence to independently controlled immutable storage when required |
| R-004 | Browser streaming | Browser WebSockets cannot send a bearer header; URL-key compatibility can leak in logs/history | Keep dashboard trusted/private; avoid query credentials where possible; rotate exposed keys |
| R-005 | Single node | Queue, tenant admission, and live fan-out are process-local | One active server per database; no horizontal scaling claim |
| R-006 | No egress broker | Namespace jobs have no supported network access; development subprocesses retain host egress | Fetch through trusted adapters and pass bounded input; never run untrusted code in subprocess mode |
| R-007 | Rootfs supply chain | Interpreter packages inside the private rootfs are trusted inputs | Build from approved snapshots, record manifests/digests, scan and patch |
| R-008 | Architecture support | The v0.2 namespace/seccomp backend supports Linux x86_64 only | Treat macOS, Windows, and non-x86_64 Linux as unisolated development platforms |
| R-009 | Forced-abort cleanup | Lease drop synchronously requests `cgroup.kill`, waits up to two seconds for `populated 0`, and removes the leaf; a hard process/host kill can still interrupt that bounded cleanup | Keep shutdown grace above worker grace; alert on and reconcile populated or stale Coop cgroups before admission |

## Receipt and event-chain semantics

A v0.2 terminal receipt is intended to bind:

- code and stdin hashes
- requested limits and nullable backend-effective controls with per-control
  enforcement metadata
- executor-observed readiness, isolation, backend, seccomp, and network posture
- accepted, started, and finished timestamps plus duration
- outcome, exit code, and kill cause; violation events are covered by the bound event-chain head
- raw drained stdout/stderr hashes, observed/retained byte counts, record counts, and truncation flags
- event-chain head, event count, and completeness
- a canonical receipt SHA-256

The digest lets the server or an auditor with the canonical fields detect accidental or unauthorized modification relative to the stored hash. It is not signed and is not anchored outside the database. A privileged database operator can rewrite records and recompute the chain. Pre-ready failures explicitly record false/null posture and controls; restart-recovered or pre-v0.2 rows with no executor observation omit that evidence rather than receiving facts inferred from current configuration.

## Verification matrix

| Property | Required evidence |
|---|---|
| Formatting and type safety | `cargo fmt --all --check`; clippy with warnings denied |
| Rust behavior | locked workspace tests on Linux, macOS, and Windows |
| Containment | ignored hostile suite on x86_64 Linux with root + writable cgroup v2; prerequisite gate must fail rather than skip |
| API tenancy | cross-tenant list/detail/result/replay/stream/cancel tests |
| Lifecycle atomicity | queued/running cancellation, crash recovery, event/state finalization tests |
| Bounded output | huge unterminated record and sustained flood tests; timeout/cancel remains responsive |
| Rootfs isolation | probes cannot read outer database, host mounts, sibling staging, or old root |
| Descendant cleanup | `setsid`/background descendants leave cgroup `populated 0` |
| SDK contracts | Python unit tests and TypeScript type/tests against v0.2 fixtures |
| API contract | generated/static OpenAPI validation and examples |
| Packaging | locked Docker build plus image/rootfs canary |
| Dependencies | RustSec advisory scan of `Cargo.lock` |
| Release integrity | tag/version check, checksums, SPDX SBOM, GitHub artifact attestation, one atomic publish job |

A skipped containment suite is not a passing result. Ordinary `cargo test` runs do not execute ignored hostile tests.

## Dependency review

The local 2026-08-27 RustSec scan evaluated 185 locked Rust dependencies and reported no known vulnerabilities. That is a point-in-time advisory lookup, not a guarantee; release CI repeats the lockfile scan and dependency updates require normal review.

## Claims intentionally not made

- VM-grade isolation or protection from kernel exploits
- deterministic re-execution
- cryptographic signing, remote attestation, or WORM audit storage
- arbitrary network egress with credential safety
- persistent workspaces or multi-node scheduling
- production support for macOS, Windows, or non-x86_64 Linux subprocess execution of untrusted code
