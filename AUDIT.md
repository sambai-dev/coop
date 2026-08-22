# Coop Security Audit — v0.1.0

- **Scope**: full workspace (`coop-types`, `coop-store`, `coop-exec`, `coop-server`), CI/CD workflows, Docker packaging, SDKs, documentation claims
- **Method**: manual adversarial code review of every request path and the sandbox implementation; RustSec advisory scan via `cargo-audit` against `Cargo.lock`; secrets scan over tracked content; automated verification via the hostile-jobs containment suite in a privileged Linux container; README/config/route claim-by-claim diffing against source
- **Result**: 4 findings fixed before release, 6 accepted with documented mitigations. Supply chain clean.

## Findings

| ID | Severity | Component | Summary | Status |
|---|---|---|---|---|
| F-001 | HIGH | API | Cross-tenant job reads (IDOR): any valid key could fetch any tenant's job view, replay, or stream by id | **FIXED** |
| F-002 | HIGH | Sandbox | cgroup attach failure was silently ignored — job would run without memory/cpu/pids caps while namespaces still applied | **FIXED** (fail-closed: child exits 126) |
| F-003 | MEDIUM | Server | Per-job broadcast channels were never freed after job completion → unbounded memory growth over uptime | **FIXED** |
| F-004 | LOW | Sandbox | Memory cap inconsistency: `RLIMIT_AS`/tmpfs sized to min(mem, 2 GiB) while `memory.max` allowed up to 4 GiB | **FIXED** (single consistent value) |
| F-005 | MEDIUM | Sandbox | No seccomp filter — syscall surface is that of the interpreter binary | Accepted; roadmap item |
| F-006 | MEDIUM | Sandbox | All jobs share UID `nobody`: `RLIMIT_NPROC` pools across concurrent jobs on one host | Accepted; per-tenant UIDs on roadmap; PID namespaces prevent cross-job process visibility |
| F-007 | DEPLOY | Ops | Namespace/cgroup backend requires root (or delegated caps); Docker path uses `--privileged` = host-equivalent trust | Documented; dedicated-VM guidance in README |
| F-008 | LOW | API | Browser WebSocket auth supports `?key=` → keys can appear in access logs/proxies | Documented; header auth preferred for non-browser clients |
| F-009 | LOW | Storage | Event log grows without retention policy | Documented TODO; SQLite single-file makes archival trivial |
| F-010 | INFO | Sandbox | `fork()` from multithreaded runtime: child performs only syscalls + small writes before `execve`, standard sandboxer pattern, residual UB risk acknowledged | Documented |

## Supply chain

```
cargo-audit v0.22.2
advisory database: 1,225 advisories
dependencies scanned: 185
vulnerabilities:   0
unmaintained:      0
unsound:           0
yanked:            0
exit code: 0
```

## Secrets & hygiene

- Pattern scan over all tracked content (GitHub/AWS/Slack/OpenAI tokens, PEM blocks): **no matches**
- Only credential material in repo is the documented dev default `COOP_API_KEYS="local:coop-dev-key"` (README instructs rotation)
- 41 tracked files; no artifacts, databases, or binaries committed (`.gitignore` covers `/target`, `*.db*`, `/data`)
- LF enforcement via `.gitattributes` (shell scripts safe cross-platform)

## Claims vs reality

| README claim | Verified by |
|---|---|
| 11 `COOP_*` env vars table | grep against `config.rs` — exact match |
| API endpoint table | grep against `routes.rs` router — exact match incl. `/openapi.json` |
| Status set (`queued…error`) | `JobStatus` enum + serde snake_case mapping |
| Hostile suite "7/7 pass in privileged CI" | CI run logs: `test result: ok. 7 passed` |
| Benchmark numbers | produced by committed `scripts/bench.py` against a local release build |

## Regression tests added with fixes

- `cross_tenant_reads_are_rejected` — second tenant gets 404 on get/replay and cannot see the job in listings
- fork-bomb probe rewritten to be *measurable*: asserts reported spawn count stays under the cap rather than assuming nonzero exit

## Residual risk statement

Coop provides defense-in-depth for *accidentally dangerous* agent code (runaway loops, bombs, network calls, file scribbles) on a machine dedicated to it. It does **not** defend against kernel-level attackers or determined multi-tenant adversaries; treat each server as a security boundary of its own until the Firecracker/gVisor backends land.
