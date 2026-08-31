# Changelog

All notable changes are documented here. Rookhold follows semantic versioning while pre-1.0; minor versions can contain API and deployment changes.

## 0.7.1 — 2026-08-31

### Hermes-inspired terminal palette

- Restyle the human CLI around a true black surface, off-white hierarchy, and one accessible electric-blue action voice informed by Hermes Agent's current visual identity.
- Lift the reference blue for readable text on dark terminals, keep `NO_COLOR` behavior unchanged, and reserve green, amber, red, and slate for explicit execution and security state.
- Regenerate the README CLI/MCP recording from the same palette and document the visual provenance without importing or vendoring another agent runtime.

## 0.7.0 — 2026-08-31

### Terminal-first operation

- Add the dependency-free `rookhold-cli` command to the Python SDK package beside `rookhold-mcp`. It supports an interactive operator prompt plus one-shot run, jobs, show, result, events, cancel, posture, and MCP-discovery commands.
- Show the authenticated tenant, live backend, observed isolation class, configured minimum, language availability, and unisolated-development warning before accepting code. API keys remain process-owned and are redacted from diagnostics.
- Keep code submission policy-bound: every CLI run sends `minimum_isolation` atomically, waits within a bounded budget, retains the job ID, and renders bounded output and terminal state.

### Universal MCP hosts

- Add copy-ready, environment-substituted stdio configurations for Claude Code and OpenCode v2 while retaining Hermes, OpenClaw, and generic MCP templates.
- Verify the CLI's MCP view by initializing the real in-process `rookhold-mcp` server and listing its live, capability-narrowed four-tool surface.
- Document that OpenCode and Claude Code own the conversational agent UI while Rookhold remains the separately controlled execution/evidence service; adding the MCP server does not disable either host's other execution tools.

### Terminal design research

- Adapt the compact banner, posture-at-a-glance, command/prompt hierarchy, restrained status color, and progressive tool-output disclosure patterns observed in the MIT-licensed OpenCode/OpenTUI terminal experience without importing its agent/session runtime or vendoring upstream source.

## 0.6.0 — 2026-08-31

### Rookhold identity

- Rename the public product, repository, dashboard, release archives, Docker image, service templates, SDK distributions, CLI commands, MCP tools, documentation, and release metadata from Coop to Rookhold.
- Make `rookhold`, `rookhold-verify`, `rookhold-sandbox-init`, `rookhold-oci-init`, `rookhold-mcp`, the `rookhold` Python module, and the `rookhold-sdk` TypeScript package the primary interfaces.
- Keep the Rust workspace crate names as an implementation namespace so the rename does not create an unnecessary internal migration.

### Compatibility and evidence stability

- Retain the legacy `coop` binaries, Python imports and class aliases, TypeScript `./coop` export, `coop-mcp` command, old MCP tool names, and every `COOP_*` configuration variable as migration aliases.
- Prefer `ROOKHOLD_*` values, fall back to `COOP_*`, and fail closed when matching non-empty old and new variables disagree.
- Preserve `/v1`, `application/vnd.coop...` media types, `coop://` subject names, evidence schema names, the predicate-v1 URI, metrics names, receipt canonicalization, event hashes, and MCP submission-reconciliation domain separators. Existing databases and signed evidence remain valid.
- Adopt existing default `coop.db`, job data, the Compose `coop-data` volume, and `.coop-runtime` state when present; new installations use Rookhold names.

### Release and documentation

- Publish the exact eight-asset v0.6 release under Rookhold archive, SDK, SBOM, checksum, attestation, and workflow-artifact names while carrying legacy executable aliases inside platform archives.
- Update the beginner quick start, agent integrations, deployment examples, systemd templates, migration guide, release checks, and production verification commands for the new identity.
- Refresh the embedded execution desk branding and its exact Content Security Policy hash without changing the operator workflow or security boundary.

## 0.5.0 — 2026-08-31

### Transcript-first execution desk

- Replace the light three-pane workbench with a carbon-dark execution desk that gives the selected run's ordered transcript primary visual weight while keeping queue state, runtime posture, and tenant context immediately available.
- Add a persistent docked run composer for Python, Node.js, and Bash with explicit minimum-isolation policy, keyboard-friendly controls, honest unavailable-runtime states, and a direct submit-to-monitor flow.
- Rework the contextual record surface around requested policy, observed execution posture, result, receipt, and signed evidence without presenting server-reported data as independently verified.

### Interaction and accessibility

- Tighten responsive queue, transcript, composer, and record behavior for desktop and narrow screens while preserving exact technical content, horizontal access, visible focus, semantic state labels, reduced motion, and high-contrast operation.
- Keep browser credentials memory-only, same-origin API and artifact access, exact inline Content Security Policy hashes, the complete six-class isolation contract, and destructive cancellation confirmation bound to the selected job identity.

### Compatibility and release

- This is an operator-interface release: the HTTP/OpenAPI contract, schema-v4 database, execution providers, security boundary, SDK behavior, and exact eight-asset supply-chain contract remain unchanged.
- The workspace and SDK version are `0.5.0`; Rust 1.98 remains the release toolchain.

## 0.4.0 — 2026-08-30

### Hardened execution and admission

- Add a per-job gVisor OCI provider with a pinned `runsc`, immutable private-rootfs manifest, nonce-bound workload readiness, cgroup limits, denied networking, complete cancel/timeout/delete handling, and crash/provider-switch reconciliation.
- Make the guarded Compose bootstrap provision and verify the reviewed runtime, rootfs digest, Ed25519 key, and gVisor host prerequisites before serving traffic; retain namespaces as an explicitly weaker shared-kernel fallback.
- Stage host-owned `runsc` and file-backed signing-key inputs into root-owned container tmpfs paths so strict production ownership checks work for a non-root Docker operator without being weakened.
- Add atomic per-tenant queue leases, fair dispatch, a weighted aggregate memory budget, transactional tenant/global logical-storage quotas, a filesystem reserve watermark, and catch-up retention.
- Make actual bound-listener validation mandatory for the binary and safe embedder API; public development credentials or subprocess execution fail closed without a separate acknowledgement.

### Identity, reliability, and protocols

- Add indexed peppered-HMAC credentials with principal, scope, expiry, and revocation metadata plus strict RFC 9068 JWT validation, bounded JWKS caching, RFC 6750 challenges, and protected-resource metadata.
- Upgrade JWT verification to patched `jsonwebtoken` 10.4 with an explicit, bundled-source AWS-LC backend, checked-in Windows NASM objects, and regress malformed registered-claim types that previously enabled type-confusion bypasses.
- Add tenant-scoped, fingerprint-bound `Idempotency-Key` submission with safe ambiguity reconciliation, typed idempotent cancellation, and OpenAPI response-header contracts.
- Add the MCP 2026 stateless discovery contract and opt-in Tasks while preserving legacy hosts; stdio requests are concurrent and cancellable, terminal isolation evidence is validated, and durable job IDs survive wait/transport ambiguity.
- Align the Python and TypeScript clients on the six-class isolation lattice, cursor/reconnect ordering, concurrent-memory capabilities, cancellation, idempotency metadata, and real-server package tests.

### Portable signed evidence

- Add the tenant-bound `coop-attestation` profile and `coop-verify` CLI: in-toto Statement/v1, exact DSSE PAE, Ed25519 signing, tenant policy, threshold verification, strict key files, frozen vectors, and a published JSON Schema.
- Advance SQLite to schema v4 with a durable signing outbox, exact deterministic tenant-bound result artifacts, immutable DSSE envelopes, quota-aware storage, exact restart reserves, tenant-scoped downloads, legacy-receipt backfill, pre-fix evidence quarantine/requeue, and conditional receipt binding.
- Expose signer capabilities, current public-key discovery with an explicit trust warning, per-job attestation metadata, and exact artifact/envelope digest headers. Signatures prove key possession and integrity, not trusted hardware or deterministic execution.
- Make the production gate invoke the packaged `coop-verify` over the exact downloaded envelope/result bytes with an explicit operator-side public-key pin; the server key endpoint remains discovery-only.

### Operations and operator experience

- Add bounded low-cardinality OpenMetrics, JSON production logs, request IDs, W3C Trace Context links, readiness caching, recovery/retention/admission metrics, and secret-free labels.
- Add scoped `whoami`, provider-aware production verification, real Python/MCP canaries, full gVisor lifecycle gates, and RustSec/package/release-surface enforcement.
- Polish the embedded workbench with minimum-isolation controls, provider/identity context, signed-evidence downloads, robust narrow-width states, keyboard/focus behavior, and memory-only browser credentials.
- Pin Rust 1.98 and include `coop-verify`, exact checksums, a combined artifact-scoped SPDX SBOM, SBOM/provenance attestations, archive inventory checks, and remote draft reconciliation in the atomic release workflow.

### Upgrade notes

- Production now requires `COOP_ATTESTATION_KEY_FILE` unless signing is explicitly disabled with `COOP_ATTESTATION_MODE=off`.
- The supported Compose path defaults to `gvisor`; rebuild the image, provision the reviewed `runsc`, regenerate the rootfs digest, and run the production verifier before restoring traffic.
- Schema v4 is forward-only. Back up schema v3 first and restore that backup instead of attempting a binary downgrade.
- The workspace and SDK version are `0.4.0`; Rust 1.98 is the release toolchain.

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
