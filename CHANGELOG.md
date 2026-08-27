# Changelog

All notable changes are documented here. Coop follows semantic versioning while pre-1.0; minor versions can contain API and deployment changes.

## 0.3.0 — 2026-08-27

### Agent integration

- Add a dependency-free `coop-mcp` stdio server to the Python SDK package.
- Expose narrow run, result, cursor-bounded evidence, and cancellation tools with MCP structured content and safety annotations.
- Keep the Coop URL, tenant key, language allowlist, wait ceiling, code-size ceiling, and required isolation posture outside model-visible arguments.
- Return a durable job ID when an adapter wait expires instead of retrying an ambiguous submission.
- Add copy-ready Hermes, OpenClaw, and generic MCP configuration with guidance for denying bypass execution tools.

### Setup and positioning

- Rewrite onboarding around the model → harness → Coop execution boundary, including explicit use/don't-use cases and the relationship to persistent harness sandboxes.
- Add a guarded dedicated-VM Compose bootstrap that creates credentials without overwriting existing secrets.
- Extract one authenticated production verifier shared by operator bootstrap and container CI; it checks configured posture and receipt evidence for Python, Node.js, and Bash canaries.

### Compatibility

- The HTTP API and v0.2 database schema remain compatible.
- The Linux x86_64 shared-kernel security boundary is unchanged; v0.3 does not add a VM or external hardened-runtime backend.

## 0.2.0 — 2026-08-26

### Security boundary

- Require a trusted private rootfs for namespace execution; host `/` is rejected.
- Move namespace/PID/rootfs bootstrap into a separately packaged, single-threaded `coop-sandbox-init` helper.
- Correct PID-namespace entry with a namespace PID 1/reaper.
- Strengthen checked privilege drop, mount setup, seccomp, and cgroup cleanup.
- Kill and drain the complete job cgroup on normal terminal paths; forced-abort lease drop synchronously requests the kill and performs drain/removal best-effort.
- Add byte-bounded output handling alongside event/line bounds.
- Linearize shutdown and per-job cancellation against helper, PID 1, and workload process creation with launch gates and nonce-bound fork acknowledgements.
- Reject blank tenants, weak production keys, unsafe job-root paths, and production seccomp disablement.
- Make unisolated production subprocess execution require an explicit unsafe acknowledgement.

### Evidence and API

- Add terminal evidence receipts with requested/effective policy, runtime posture, lifecycle/outcome fields, output digests, and receipt SHA-256.
- Derive receipt isolation and bootstrap posture from the executor's observed ready boundary; pre-ready and restart-recovery failures no longer inherit configured containment claims.
- Separate requested limits from backend-effective controls. The development subprocess reports only wall-time enforcement; unsupported or unactivated controls remain explicit null values with per-control enforcement metadata.
- Probe development runtimes once at startup, advertise only successful canaries, reuse their exact executable paths, and reject unavailable runtimes with `422 runtime_unavailable`.
- Add tamper-evident per-job event chaining and verification metadata. Hashes are server-verifiable, not signatures or WORM proof.
- Make submission commits cancellation-safe after SQLite dispatch, reconcile ambiguous commit acknowledgements, and keep admission leases through durable scheduler handoff.
- Improve cancellation/finalization consistency, fair bounded scheduling, shutdown failure propagation, and stream/result handling.
- Tighten OpenAPI, error, pagination/filter, and status/capability descriptions.

### Operator experience

- Replace the dashboard with a boundary-first run explorer focused on lifecycle, output, events, policy, and receipts.
- Serialize dashboard evidence reads under transfer limits and serve a hash-only CSP plus anti-framing, MIME-sniffing, and referrer protections.
- Package and test the Python and TypeScript clients; include Python wheel, Python source distribution, and npm tarball assets without claiming registry publication.
- Normalize SDK deadlines, retries, truncated responses, pagination, cancellation, and one-use WebSocket ticket behavior into typed client contracts.
- Rework onboarding, deployment, security-boundary, operations, backup/restore, upgrade, and troubleshooting documentation.
- Add a small Docker build context, pinned Rust toolchain and base-image digests, an immutable Debian package snapshot shared by the outer runtime and private rootfs, a locked image build, and a health check.
- Make the benchmark use result waits and rate-limit backoff instead of 20 ms polling.
- Harden CI/release permissions and gates; publish checksums, an SPDX SBOM, and build provenance.

### Breaking changes

- The v0.2 namespace/seccomp backend supports Linux x86_64 only; other platforms are unisolated development targets.
- Namespace deployments must set `COOP_ROOTFS`.
- v0.1 deployment/security claims are superseded by [SECURITY.md](SECURITY.md) and [docs/security-boundary.md](docs/security-boundary.md).
- Pre-v0.2 events may not have complete receipt/event-chain verification metadata.
- `effective_spec` now uses nullable `EffectiveLimits`; execution policy, capabilities, and receipts add readiness/isolation and per-control enforcement metadata.

## 0.1.0 — 2026-08-22

Initial public release. This version is no longer supported for hostile or multi-tenant execution.
