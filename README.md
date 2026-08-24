# Coop

**A self-hostable sandbox that safely executes untrusted code and tool calls on behalf of AI agents.**

[![ci](https://github.com/sambai-dev/coop/actions/workflows/ci.yml/badge.svg)](https://github.com/sambai-dev/coop/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/sambai-dev/coop)](https://github.com/sambai-dev/coop/releases)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Resource limits, live streaming output, and a replayable audit log. One Rust binary, one SQLite file, no cloud dependency.

```bash
# option 1: prebuilt binary (linux-musl / macOS arm64 / windows x64)
curl -sL https://github.com/sambai-dev/coop/releases/latest/download/coop-x86_64-unknown-linux-musl.tar.gz | tar xz

# option 2: from source, or docker
cargo install --path crates/coop-server   # or: docker compose up
COOP_API_KEYS="local:my-key" coop
```

Every team building agents hits the same wall: *the agent wants to run code — where does it actually run?* Most teams hack together Docker containers with no CPU/memory caps, no observability, and no audit trail. Managed options (E2B, Modal) are great but add cost, latency, and a third party to your trust boundary. Coop is the small, honest, self-hosted answer.

## How Coop compares

| | Coop | E2B / Modal (managed) | Raw Docker scripts | gVisor / Firecracker DIY |
|---|---|---|---|---|
| Data leaves your network | **never** | yes | never | never |
| Setup effort | one binary + SQLite file | account + SDK + egress | hours of glue code | days of infra |
| CPU/mem/pid limits per job | ✅ enforced, fail-closed | ✅ | usually not | ✅ |
| Live output streaming | ✅ WS, replayable | ✅ | build it yourself | build it yourself |
| Replayable audit log of every run | ✅ append-only event store | partial (dashboard only) | ❌ | ❌ |
| Runs on any $5 VPS | ✅ Linux, root | ❌ (their cloud) | ✅ | needs KVM/nested virt |
| License | MIT | proprietary | — | Apache-2.0 |

The honest tradeoff: Coop is namespaces+cgroups defense-in-depth, **not** a VM boundary — see the [containment table](#isolation-strategy-stated-honestly). If you need kernel-level isolation today, put Coop inside a Firecracker VM (or wait for the microVM backend on the roadmap).

---

## The 60-second tour

```bash
git clone https://github.com/sambai-dev/coop && cd coop
COOP_API_KEYS="local:dev-key" cargo run -p coop-server
# open http://127.0.0.1:7300 for the live dashboard
```

Submit a job, stream its output while it runs:

```bash
curl -s -X POST localhost:7300/v1/jobs \
  -H "Authorization: Bearer dev-key" -H "Content-Type: application/json" \
  -d '{"language":"python","code":"print(\"hello from a sandbox\")","limits":{"wall_seconds":10,"mem_mb":256}}'
# {"job_id":"01a02...","status":"queued", ...}
```

```bash
curl -s localhost:7300/v1/jobs/<id>        -H "Authorization: Bearer ***"   # status + exit code
curl -s localhost:7300/v1/jobs/<id>/replay -H "Authorization: Bearer ***"   # full event history
curl -s "localhost:7300/v1/jobs/<id>/result?wait_seconds=60" -H "Authorization: Bearer ***"   # one-call result for agents
websocat "localhost:7300/v1/jobs/<id>/stream?key=dev-key"                       # live stdout/stderr frames
```

Try to break it on Linux:

```bash
curl -s -X POST localhost:7300/v1/jobs -H "Authorization: Bearer dev-key" \
  -H "Content-Type: application/json" \
  -d '{"language":"python","code":"while True: pass","limits":{"wall_seconds":3}}'
# → timed_out, killed at t=3s, host unharmed
```

---

## Architecture

```
┌─────────────┐   HTTP/WS    ┌──────────────────────────────┐
│  Agent/SDK  │ ───────────▶ │  API Gateway (axum)          │
└─────────────┘              │  - bearer auth (API keys)    │
                             │  - fixed-window rate limit   │
                             │  - job submission            │
                             └──────────┬───────────────────┘
                                        │ mpsc queue
                             ┌──────────▼───────────────────┐
                             │  Scheduler (tokio workers)   │
                             │  - per-tenant concurrency    │
                             │    semaphores                │
                             └──────────┬───────────────────┘
                                        │ spawn
                             ┌──────────▼───────────────────┐
                             │  Executor                    │
                             │  - Linux: namespaces +       │
                             │    cgroup v2 + rlimits       │
                             │  - dev fallback: plain       │
                             │    subprocess + wall clock   │
                             │  - stdout/stderr streamed    │
                             └──────────┬───────────────────┘
                                        │ events
                             ┌──────────▼───────────────────┐
                             │  Event Log (SQLite)          │
                             │  append-only, replayable     │
                             └──────────┬───────────────────┘
                                        │ broadcast + WS push
                             ┌──────────▼───────────────────┐
                             │  Dashboard (served by the    │
                             │  binary, zero build step)    │
                             └──────────────────────────────┘
```

Workspace layout:

| crate | role |
|---|---|
| `coop-types` | job specs, limits (server-side clamped), status enums |
| `coop-store` | SQLite jobs + append-only `events` table |
| `coop-exec` | executor backends behind one `execute()` signature |
| `coop-server` | axum gateway, scheduler, WS fan-out, dashboard, OpenAPI |

## Design decisions

### Isolation strategy, stated honestly

On Linux with root, Coop runs each job inside:

- **namespaces**: mount (read-only bind-remount of `/`, private tmpfs at `/tmp`), PID, network (`CLONE_NEWNET` = no interfaces at all unless explicitly allowed later), IPC, UTS
- **cgroup v2**: `memory.max`, `memory.swap.max=0`, `cpu.max`, `pids.max`
- **rlimits**: `CPU` (SIGXCPU), `AS`, `NPROC`, `NOFILE`, `FSIZE`
- **privilege drop** to `nobody` when started as root, fresh `/proc`, minimal env

| Defended against | Not defended against (yet) |
|---|---|
| fork bombs (`pids.max`, `RLIMIT_NPROC`) | kernel 0-days / container escapes |
| memory bombs (cgroup OOM kill, hard `RLIMIT_AS`) | side channels / timing attacks |
| infinite loops & CPU hogs (wall clock + `RLIMIT_CPU`) | malicious interpreter CVEs |
| disk fill (tmpfs size cap, `RLIMIT_FSIZE`) | sophisticated syscall-level attacks (no seccomp yet) |
| network access by default (no netns interfaces) | multi-tenant hostile neighbors on one host |
| read-only rootfs tampering, host file reads | |

Run Coop on a **dedicated VM**, not your workstation, and treat it as defense-in-depth rather than a hard security boundary. gVisor/Firecracker backends behind the same API are the roadmap answer to the right-hand column.

If you start `coop` without root (or on macOS/Windows dev machines) it falls back to plain subprocess execution with wall-clock timeouts and says so in `/healthz` (`"sandbox": "off"`) and in the logs. We would rather advertise weakness than fake strength.

Two deliberate engineering choices worth calling out:

- **Direct cgroupfs writes instead of a cgroup wrapper crate.** The v2 interface is four small files (`memory.max`, `cpu.max`, `pids.max`, `cgroup.procs`). Writing them directly gives deterministic control over exactly which knobs we set, works under systemd delegation, and drops a dependency whose abstraction we'd have to fight anyway.
- **`fork()` + `execve()` with pre-built argv/env.** All CStrings, paths, and cgroup setup happen before the fork; the child only does async-signal-safe-adjacent syscalls (unshare/mount/rlimits/exec). This is the same shape ion-style sandboxes use; the MT-fork caveats are documented and bounded because the child never allocates meaningfully before `exec`.

### Streaming is the product

stdout/stderr are read line-by-line as the process runs, appended to the event log, and fanned out over a per-job `broadcast` channel to every WebSocket subscriber. A client that connects mid-run first receives the persisted history (deduped by sequence number, lag recovery included), then live frames, then a `finished` frame. You never miss output and you never see duplicates.

### Every execution is replayable

Each job produces an ordered event stream: `started → stdout/stderr… → violation? → truncated? → finished{status, exit_code, duration_ms}`. Events are append-only rows in SQLite keyed by `(job_id, seq)`. `GET /v1/jobs/{id}/replay` returns the full record; the WS stream replays the same data. Deterministic job records mean post-mortems ("what did the agent actually run and print at 14:03?") are a query, not an archaeology project.

### SQLite first

One file, zero ops, WAL mode, safe concurrent readers. The store is a thin module (`coop-store`) so swapping in Postgres for horizontal scale is a bounded change, not a rewrite.

### Limits are clamped server-side

Clients propose `limits`; the server clamps them against ceilings (`wall ≤ 300s`, `mem ≤ 4GiB`, `pids ≤ 1024`, …). A hostile tenant cannot buy their way past safety with a bigger JSON payload.

## API

All endpoints require `Authorization: Bearer <key>` (or `?key=` for browser WebSockets). Machine-readable OpenAPI at `/openapi.json`.

| Method | Path | Purpose |
|---|---|---|
| POST | `/v1/jobs` | submit `{language, code, stdin?, limits?}` → `201 {job_id}` |
| GET | `/v1/jobs?limit=` | list your tenant's recent jobs |
| GET | `/v1/jobs/{id}` | job view (status, exit code, timestamps) |
| DELETE | `/v1/jobs/{id}` | cancel a queued/running job (409 if already terminal) |
| GET | `/v1/jobs/{id}/replay` | full ordered event list |
| GET | `/v1/jobs/{id}/result` | one-call outcome: waits up to `?wait_seconds=` (0-300, default 60), folds stdout/stderr/violations into `{status, exit_code, duration_ms, stdout, stderr, truncated, violations}`; `200` when terminal, `202` with partial output if the wait budget expires |
| GET | `/v1/jobs/{id}/stream` | WebSocket: history + live events |
| GET | `/v1/metrics` | Prometheus text format (job counts, running jobs) |
| GET | `/healthz` | liveness (`{"ok":true}`, no auth) |
| GET | `/v1/status` | version + active sandbox mode |

Statuses: `queued → running → succeeded | failed | timed_out | oom_killed | cancelled | error`.

Languages: `python`, `node`, `bash` (interpreter binaries configurable via env).

## Wiring it to an agent

The point of Coop is being the place your agent's code actually runs. The loop is one call on either side:

```
┌──────────────┐   "run this python"   ┌─────────────────┐
│  Your agent  │ ────────────────────▶ │      Coop       │
│ (any LLM/    │ ◀──────────────────── │ sandbox + audit │
│  framework)  │    one-call result     └─────────────────┘
└──────────────┘
```

Python (`sdks/python/coop.py` — the same stdlib-only file as above):

```python
from coop import Coop
coop = Coop("http://sandbox.internal:7300", "tenant-key")

def run_agent_code(code: str) -> str:
    job = coop.submit("python", code, limits={"wall_seconds": 30, "mem_mb": 256})
    return coop.result(job["job_id"])          # one call: waits + returns {status, exit_code, stdout, stderr}

# wherever your tool-calling loop executes code:
#   tool_output = run_agent_code(llm_tool_call["code"])
```

Every run lands in the audit log regardless of what the agent does — including the
ones that get killed mid-flight. Point your agent at a Coop host instead of
`exec()`, and "what did the model run?" becomes a SQL query.

## SDKs (one file each)

Python (`sdks/python/coop.py`, stdlib only):

```python
from coop import Coop
coop = Coop("http://127.0.0.1:7300", "dev-key")
print(coop.result(coop.submit("python", "print(6*7)")["job_id"]))
for event in coop.stream(job_id): ...
```

TypeScript (`sdks/typescript/coop.ts`, fetch + native WebSocket):

```ts
const coop = new Coop("http://127.0.0.1:7300", "dev-key");
console.log(await coop.result((await coop.submit("node", "console.log(6*7)")).job_id));
coop.stream(jobId, (e) => console.log(e.kind, e.data));
```

## The hostile-jobs suite

The portfolio piece isn't the happy path — it's proof that the unhappy path is contained. `hostile-jobs/` plus `crates/coop-server/tests/hostile.rs` assert real containment on Linux, and CI runs them in a privileged container on every push — **all 7 currently pass**:

| Job | Expectation |
|---|---|
| `fork_bomb.sh` | dies fast under `pids.max`/`NPROC`; server keeps serving afterwards |
| `memory_bomb.py` | `oom_killed` or allocation failure, never takes the host down |
| `infinite_loop.py` | `timed_out` at the wall clock, not a millisecond later |
| `network_probe.py` | exits clean only if the network really is unreachable |
| `disk_filler.py` | fails against tmpfs cap + `RLIMIT_FSIZE` |
| `escape_probe.py` | cannot read `/etc/shadow` or write outside its box |
| `pid_bomb.py` | process-spawn storm capped |

Run them (root required, namespaces + cgroup v2):

```bash
sudo cargo test -p coop-server --test hostile -- --ignored --nocapture
```

CI runs this in a dedicated privileged job on every push.

## Numbers

Measured with `scripts/bench.py` (end-to-end submit → terminal state), release build:

| setup | concurrency | throughput | p50 | p95 | p99 |
|---|---|---|---|---|---|
| dev laptop, Windows, release build, naive subprocess backend | 1 | 18.1 jobs/s | 49 ms | 78 ms | 81 ms |
| same | 4 | 41.1 jobs/s | 90 ms | 167 ms | 200 ms |

Honest footnotes: these are a single laptop where the dominant cost is Python interpreter startup (~40 ms); Linux servers will be better, and the namespace backend adds a small constant for mount/unshare work. Cold-start latency is interpreter-dominated, which is exactly why snapshot/warm-pool warm starts are on the roadmap. Every job across both runs reached `succeeded`. Re-run on your hardware:

```bash
python scripts/bench.py --url http://your-host:7300 --key YOUR_KEY --jobs 100 --concurrency 8
```

We publish the harness instead of cherry-picked screenshots. Replace this table with your numbers and open a PR.

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `COOP_ADDR` | `127.0.0.1:7300` | listen address |
| `COOP_DB` | `coop.db` | SQLite path |
| `COOP_API_KEYS` | `local:coop-dev-key` | comma list of `tenant:key` or bare keys |
| `COOP_WORKERS` | `4` | executor worker tasks |
| `COOP_TENANT_CONCURRENCY` | `2` | max parallel jobs per tenant |
| `COOP_RATE_PER_MIN` | `120` | requests/min per tenant |
| `COOP_RETENTION_HOURS` | `168` | delete terminal jobs (and events) older than this; `0` disables sweeping |
| `COOP_SWEEP_INTERVAL_SECS` | `3600` | seconds between retention sweeps |
| `COOP_ENV` | unset | `prod`/`production`/`release` enables production fail-fast checks (require real API keys, refuse to boot without sandbox) |
| `COOP_SANDBOX` | `auto` | `auto` \| `ns` \| `off` |
| `COOP_JOBS_ROOT` | `/var/lib/coop/jobs` (Linux) | scratch dir for job scripts; must live outside any tmpfs the sandbox overlays |
| `COOP_PYTHON` / `COOP_NODE` / `COOP_BASH` | PATH lookup | interpreter overrides |
| `RUST_LOG` | `info` | e.g. `debug`, `coop_server=trace` |

## Deployment

```bash
export COOP_API_KEYS="tenant:$(openssl rand -hex 16)"   # required — compose fails fast without it
docker compose up          # privileged: true is what enables the ns/cgroup backend
```

Two deploy defaults are deliberately strict in `docker-compose.yml`:

- **No API key ships in the repo.** `COOP_API_KEYS` must come from your environment; compose aborts immediately if it is unset, and the container (`COOP_ENV=production`) refuses to boot on the development default key. One key per agent/tenant keeps blast radius small.
- **Localhost-only publish.** The port mapping is `127.0.0.1:7300:7300`, and the Dockerfile's `0.0.0.0` bind only listens inside the container's network namespace. Coop speaks plain HTTP with bearer keys and has no TLS of its own, so to reach it from other machines put a TLS-terminating front proxy (nginx, Caddy, Traefik) on the public interface and proxy to `127.0.0.1:7300`. Do not rebind `0.0.0.0` on the host directly — API keys would cross the network in plaintext.

Hardening checklist for production-ish use:

- dedicated VM (Firecracker/gVisor integration is the stretch goal)
- one key per agent/tenant for blast-radius isolation; rotate on any suspicion of leak
- keep `COOP_DB` on persistent storage; the audit log is the point
- firewall egress from the host itself; jobs already have none

## Security and audit

A full security audit shipped with v0.1.0 — see **[AUDIT.md](AUDIT.md)** for the complete report and **[SECURITY.md](SECURITY.md)** for the reporting policy.

- 4 findings found and fixed before release, including two high-severity ones: cross-tenant job reads (IDOR) and a silent cgroup-attach failure that could have run jobs without memory/cpu/pid caps
- supply chain clean: `cargo-audit` over 185 dependencies — 0 vulnerabilities, 0 unmaintained, 0 unsound, 0 yanked
- secrets scan over tracked content: clean; regression tests added for every fixed finding

## CI and releasing

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on every push
- privileged `hostile` CI job proving containment on Linux runners (currently 7/7 green)
- tag `v*` → release binaries for linux-musl, macOS arm64, Windows x64 (v0.1.0 is live on the Releases page)
- crates.io: crates are publish-ready; run `cargo publish -p coop-types`, then `-p coop-exec`, `-p coop-store`, `-p coop-server` once a token is configured

## Roadmap

- [x] Week 1 — naive executor: submit → subprocess → timeout → output, integration tests
- [x] Week 2 — namespaces + cgroups v2 + rlimits, hostile-jobs containment suite
- [x] Week 3 — WebSocket streaming, SQLite event log, replay endpoint
- [x] Week 4 — API keys, per-tenant rate/concurrency limits, live dashboard
- [x] Week 5 — Docker deploy, SDKs, benchmarks, this README
- [ ] seccomp allowlist profiles per language
- [ ] Redis-backed queue for multi-node schedulers; Postgres store
- [ ] resource graphs in the dashboard (CPU/memory sampled per job)
- [ ] Firecracker microVM backend behind the same API; VM snapshotting for ~5 ms warm starts
- [ ] OpenTelemetry export alongside local tracing spans

## License

MIT — same spirit as the tools that inspired the workflow.
