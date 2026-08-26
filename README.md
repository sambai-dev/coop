# Coop

**Run agent code under policy. Keep the evidence.**

[![CI](https://github.com/sambai-dev/coop/actions/workflows/ci.yml/badge.svg)](https://github.com/sambai-dev/coop/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sambai-dev/coop)](https://github.com/sambai-dev/coop/releases)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Coop is a self-hosted execution gateway for short-lived Python, Node.js, and Bash jobs. It authenticates tenants, clamps requested resources to operator policy, streams bounded output, and stores an ordered execution record plus a terminal receipt in SQLite.

Use Coop when you need defensible answers to five questions: *what ran, who submitted it, which controls actually became effective, what output or violations were observed, and how did the run end?*

Coop is deliberately narrower than a persistent cloud development environment. It does not currently provide long-lived workspaces, arbitrary images, browser sessions, port forwarding, snapshots, or a VM boundary.

> **Security boundary:** v0.2 never uses the host `/` as a job root. The in-tree Linux x86_64 backend requires a dedicated private rootfs, namespaces, cgroup v2, rlimits, privilege dropping, and an x86_64 seccomp policy. It is still a shared-kernel boundary and belongs on a dedicated VM. macOS, Windows, and other Linux architectures can run only the plain subprocess backend, which is for same-trust development. Read [the security boundary](docs/security-boundary.md) before accepting untrusted jobs.

> **Current release:** [v0.2.0](https://github.com/sambai-dev/coop/releases/tag/v0.2.0). Release assets include checksums, an SPDX SBOM, and build provenance. v0.1.x is unsupported for hostile or multi-tenant execution.

## Why Coop

- **Observed policy, not configuration claims.** Receipts distinguish requested limits from the controls the selected backend actually enforced.
- **Evidence survives unsuccessful runs.** Ordered lifecycle, output, violation, and outcome records remain inspectable after failure, timeout, OOM, or cancellation.
- **Live without being ephemeral.** WebSocket streaming is backed by persisted history, so clients can reconnect and resume from a cursor.
- **A control plane you can own.** The API, dashboard, scheduler, and SQLite evidence store run as one self-hosted service.

### Included in v0.2

- one authenticated HTTP API for submit, inspect, cancel, wait, and event history
- live WebSocket output with one-use stream tickets and persisted history before live frames
- per-tenant API keys, rate limits, and concurrency limits
- server-clamped wall-time, CPU, memory, process, and file limits enforced by the namespace backend
- a private-rootfs Linux x86_64 execution backend with job networking denied
- an operator dashboard served from the binary
- a SQLite job/event store with configurable retention and per-job hash chains
- terminal evidence receipts binding policy, runtime posture, output digests, outcome, and chain head
- stdlib-only Python and dependency-free TypeScript clients

The evidence log is an operational record with server-verifiable SHA-256 links and terminal receipt hashes. It is not signed or externally anchored: an administrator who can rewrite the database can recompute it. Coop does not claim deterministic re-execution, remote attestation, or WORM storage.

## Quick start

### Local development

Install Rust 1.89 and whichever of Python 3, Node.js, and Bash you intend to
run, then:

```bash
git clone https://github.com/sambai-dev/coop.git
cd coop
COOP_SANDBOX=off \
COOP_JOBS_ROOT="$PWD/.coop-dev/jobs" \
cargo run --locked -p coop-server
```

PowerShell:

```powershell
git clone https://github.com/sambai-dev/coop.git
Set-Location coop
$env:COOP_SANDBOX = "off"
$env:COOP_JOBS_ROOT = Join-Path (Get-Location) ".coop-dev\jobs"
cargo run --locked -p coop-server
```

Open <http://127.0.0.1:7300>. Development mode uses the public local key `coop-dev-key` if `COOP_API_KEYS` is unset. The explicit `off` setting above uses an **unisolated subprocess**. Do not expose it or submit code you do not trust.

At startup, development mode runs a bounded canary under the same sanitized
environment used for jobs. `/v1/capabilities` advertises only runtimes that
passed, and submissions for an unavailable runtime fail with
`422 runtime_unavailable`. The resolved executable is cached for the process,
so admission and execution use the same runtime path.

### Dedicated Linux x86_64 VM

The supplied Compose deployment includes a purpose-built private rootfs and starts Coop in production mode:

```bash
if [ -e .env ]; then
  echo ".env already exists; refusing to overwrite a secrets file" >&2
else
  install -m 0600 .env.example .env
  key="$(openssl rand -hex 32)"
  sed -i "s/^COOP_API_KEYS=.*/COOP_API_KEYS=agent-a:${key}/" .env
  export COOP_CLIENT_KEY="$key"
  docker compose up --build -d --wait
  docker compose ps
fi
```

The guard creates the bearer-key file with mode `0600` and never overwrites an existing `.env`. If the file already exists, review it, start Compose separately with the same `--wait` command, and export its key as `COOP_CLIENT_KEY`.

Compose is loopback-only, but its namespace backend currently requires a privileged container on an x86_64 Linux host. `privileged: true` is host-equivalent authority: use this configuration only inside a dedicated, disposable VM. It is not a safe multi-tenant boundary on a general-purpose Docker host. See [deployment choices](docs/deployment.md).

## Run a job

Set the client key for the path you started: use the public development key only for the loopback local-development process, or use the random key portion you placed after `tenant:` in `.env` for Compose.

```bash
COOP_CLIENT_KEY="${COOP_CLIENT_KEY:-coop-dev-key}"
curl --fail-with-body -X POST http://127.0.0.1:7300/v1/jobs \
  -H "Authorization: Bearer $COOP_CLIENT_KEY" \
  -H 'Content-Type: application/json' \
  --data '{
    "language": "python",
    "code": "print(6 * 7)",
    "limits": {"wall_seconds": 10, "mem_mb": 256}
  }'
```

The response contains a UUIDv7 `job_id`. Use it in the following requests:

```bash
curl --fail-with-body \
  -H "Authorization: Bearer $COOP_CLIENT_KEY" \
  'http://127.0.0.1:7300/v1/jobs/JOB_ID/result?wait_seconds=60'

curl --fail-with-body \
  -H "Authorization: Bearer $COOP_CLIENT_KEY" \
  http://127.0.0.1:7300/v1/jobs/JOB_ID/replay

stream_response=$(curl --fail-with-body -X POST \
  -H "Authorization: Bearer $COOP_CLIENT_KEY" \
  http://127.0.0.1:7300/v1/jobs/JOB_ID/stream-ticket)
stream_path=$(printf '%s' "$stream_response" | \
  python -c 'import json, sys; print(json.load(sys.stdin)["stream_url"])')
websocat "ws://127.0.0.1:7300${stream_path}"
```

PowerShell local-development equivalent:

```powershell
$headers = @{ Authorization = "Bearer coop-dev-key" }
$body = @{
    language = "python"
    code = "print(6 * 7)"
    limits = @{ wall_seconds = 10; mem_mb = 256 }
} | ConvertTo-Json -Depth 3
$job = Invoke-RestMethod -Method Post -Headers $headers `
    -ContentType "application/json" -Body $body `
    -Uri "http://127.0.0.1:7300/v1/jobs"
$result = Invoke-RestMethod -Headers $headers `
    -Uri "http://127.0.0.1:7300/v1/jobs/$($job.job_id)/result?wait_seconds=60"
$result
```

Prefer `/result` to status polling. Resume replay and WebSocket streams from the last accepted cursor after a transport close. Do not automatically retry a timed-out submission because it may already have committed; see [API and streaming](docs/api.md) for the complete transport contract.

## Execution lifecycle

```text
accepted → queued → running → succeeded
                            ↘ failed
                            ↘ timed_out
                            ↘ oom_killed
                            ↘ cancelled
                            ↘ error
```

Each job has an ordered event history. A client that joins the WebSocket after execution began receives persisted events first and then live events. Output is bounded; truncation is recorded rather than allowing an unbounded server-memory or database write path.

## API and clients

OpenAPI is served at `/openapi.json`. The core routes are:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/jobs` | Submit a job |
| `GET` | `/v1/jobs` | Cursor-list the authenticated tenant's jobs |
| `GET` | `/v1/jobs/{id}` | Inspect status, requested/effective policy, and terminal receipt |
| `DELETE` | `/v1/jobs/{id}` | Cancel a queued or running job |
| `GET` | `/v1/jobs/{id}/result` | Wait for and fold an outcome |
| `GET` | `/v1/jobs/{id}/replay` | Cursor-read ordered persisted events |
| `GET` | `/v1/jobs/{id}/stream` | WebSocket history plus live events |
| `POST` | `/v1/jobs/{id}/stream-ticket` | Mint a short-lived, one-use, job-bound stream credential |
| `GET` | `/v1/status` | Authenticated build and sandbox posture |
| `GET` | `/v1/capabilities` | Supported languages, limits, and server features |
| `GET` | `/v1/whoami` | Resolve the current key's tenant |
| `GET` | `/v1/metrics` | Prometheus-format process/job metrics |
| `GET` | `/healthz` | Unauthenticated liveness only |
| `GET` | `/readyz` | Unauthenticated process/store readiness; still verify authenticated posture |

See [API and streaming](docs/api.md) and [SDK usage](docs/sdks.md). The dashboard uses the same API; it is an operator surface, not a separate source of truth.

## Configuration

| Variable | Default | Notes |
|---|---|---|
| `COOP_ADDR` | `127.0.0.1:7300` | Listen address; keep private or place behind TLS |
| `COOP_DB` | `coop.db` | SQLite database path |
| `COOP_API_KEYS` | dev key outside production | Comma-separated `tenant:key` entries; production rejects blank, short, and public keys |
| `COOP_ENV` | unset | `prod`, `production`, or `release` enables fail-closed production checks |
| `NODE_ENV` | unset | Compatibility alias: `prod`, `production`, or `release` also enables the same production checks |
| `COOP_SANDBOX` | `auto` | `auto`, `ns`, or `off`; production does not silently downgrade |
| `COOP_ROOTFS` | unset | Required private rootfs for the namespace backend; `/` is rejected |
| `COOP_SANDBOX_HELPER` | sibling `coop-sandbox-init` | Dedicated single-threaded Linux x86_64 bootstrap helper; package and version it with `coop` |
| `COOP_UNSAFE_ALLOW_NAIVE` | false | Required acknowledgement for an explicit unisolated production-mode process |
| `COOP_SECCOMP` | `auto` | Namespace syscall filter; cannot be disabled in production |
| `COOP_JOBS_ROOT` | `/var/lib/coop/jobs` on Linux | Dedicated absolute non-symlink staging directory |
| `COOP_WORKERS` | `4` | Worker count |
| `COOP_TENANT_CONCURRENCY` | `2` | Concurrent jobs per tenant |
| `COOP_RATE_PER_MIN` | `120` | Requests per minute per tenant |
| `COOP_RETENTION_HOURS` | `168` | Terminal-job retention; `0` disables deletion |
| `COOP_SWEEP_INTERVAL_SECS` | `3600` | Retention sweep interval, minimum 60 |
| `COOP_PYTHON`, `COOP_NODE`, `COOP_BASH` | `PATH` lookup | Interpreter overrides; paths must exist in the private rootfs too |
| `RUST_LOG` | `info` | Rust tracing filter |

Requested limits are clamped to compiled ceilings before execution, but
"requested" is not the same as "enforced." The namespace backend enforces the
clamped wall-time, CPU, memory, process, and file controls. The unisolated
development subprocess backend enforces only wall time; its CPU, memory,
process, and file values are `null` in effective policy and their
`limit_enforcement` flags are `false`. `allow_network` is not an egress opt-in
in v0.2: the namespace backend denies job networking, while the development
backend retains the service account's host networking and reports
`networking: "host"` after the workload reaches its ready boundary.

## Repository map

| Path | Responsibility |
|---|---|
| `crates/coop-types` | API types, statuses, and limit ceilings |
| `crates/coop-store` | SQLite jobs and ordered events |
| `crates/coop-exec` | development executor and Linux x86_64 namespace executor |
| `crates/coop-server` | API, scheduler, authentication, dashboard, OpenAPI |
| `sdks` | Python and TypeScript clients |
| `hostile-jobs` | adversarial containment probes |
| `docs` | architecture, boundary, API, deployment, and operations |

## Verification

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
python scripts/check-release-surface.py
python -m pip install --no-deps ./sdks/python
python -m unittest discover -s sdks/python/tests -v
cd sdks/typescript
npm ci
npm test
npm run typecheck
```

Containment tests and the v0.2 namespace backend are Linux x86_64-only. They require a kernel with `cgroup.kill` and recursive `mount_setattr` support (use Linux 5.14 or newer), root, cgroup v2, namespace support, the matching helper, and a trusted private rootfs. After preparing the rootfs as described in [deployment](docs/deployment.md), run from a root-owned x86_64 test environment with Rust 1.89 available:

```bash
sudo env \
  COOP_ROOTFS=/opt/coop/rootfs \
  COOP_SANDBOX_HELPER=/usr/local/bin/coop-sandbox-init \
  cargo test --locked -p coop-server --test hostile -- --ignored --nocapture
```

A successful unit test run on macOS, Windows, or another Linux architecture is not evidence that Linux x86_64 containment works. Those platforms use the unisolated development subprocess backend only. Release CI constructs an ephemeral x86_64 private rootfs, expects exactly 18 hostile tests, checks every prerequisite, and fails if the suite cannot run or reports a skip.

## Documentation

- [Architecture](docs/architecture.md)
- [Security boundary and trust tiers](docs/security-boundary.md)
- [API and streaming](docs/api.md)
- [SDKs](docs/sdks.md)
- [Deployment](docs/deployment.md)
- [Operations, backup, and restore](docs/operations.md)
- [Upgrading](docs/upgrading.md)
- [Releasing](docs/releasing.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Security policy](SECURITY.md)
- [Security review record](AUDIT.md)
- [Changelog](CHANGELOG.md)

Runnable starting templates for systemd, its environment file, and Caddy live under [`deploy/`](deploy/).

## Project direction

Coop's next priorities are an external hardened-runtime adapter such as gVisor/OCI, signed or externally anchored receipts, credential-brokered outbound access, resource time-series, and framework adapters. Coop v0.2 does not claim these capabilities or a microVM boundary.

## License

[MIT](LICENSE)
