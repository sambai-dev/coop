# Security policy

## Reporting a vulnerability

Email `sambai.codes@gmail.com` with a subject beginning `[rookhold security]`. If GitHub private vulnerability reporting is enabled for the repository, the **Security** tab's **Report a vulnerability** form is also acceptable. Do not include exploit details, tenant data, credentials, or unredacted execution evidence in a public issue.

Include, when safe:

- affected commit/version and deployment mode
- host kernel, cgroup mode, container/runtime, and architecture
- selected sandbox posture from `/v1/status`
- a minimal reproducer and the expected versus observed boundary
- whether data, host access, cross-tenant access, or availability was affected
- logs with API keys, source, stdin, and customer output removed

We aim to acknowledge a complete report within 72 hours. Timelines for validation and remediation depend on severity and reproducibility. Reporters may request credit or anonymity.

## Supported versions

| Version | Security support |
|---|---|
| latest `main` | Active development; may include unreleased changes |
| tagged `0.7.x` releases | Supported |
| tagged `0.6.x` releases | Unsupported; upgrade to 0.7.x |
| tagged `0.5.x` releases | Unsupported; upgrade to 0.7.x |
| tagged `0.4.x` releases | Unsupported; upgrade to 0.7.x |
| tagged `0.3.x` releases | Unsupported; upgrade to 0.7.x |
| tagged `0.2.x` releases | Unsupported; upgrade to 0.7.x |
| `0.1.x` | Unsupported; do not expose to hostile tenants |
| older versions | Unsupported |

`main` can describe changes not yet present in a tagged artifact. A source checkout is not a release artifact; use a commit digest when reporting it.

## Boundary summary

Rookhold v0.7 retains three materially different execution modes:

- `gvisor`: the guarded production default on Linux x86_64. Every job receives a separate OCI workload under the reviewed, digest-pinned `runsc` application kernel, an immutable private-rootfs manifest, cgroup v2/rlimits, a dedicated OCI init, and denied networking.
- `namespaces`: a weaker shared-host-kernel Linux x86_64 fallback requiring a private rootfs, cgroup v2, rlimits, the x86_64 seccomp policy, checked privilege drop, and isolated namespaces. Run it only on a dedicated x86_64 VM.
- `off`: a plain subprocess for same-trust development. It is not a sandbox,
  submitted code has the service account's authority, and only wall time (not
  requested CPU, memory, process-count, or file-size limits) is enforced.

Isolated execution is unsupported on macOS, Windows, and non-x86_64 Linux; those platforms can use only the unisolated development subprocess backend. Rookhold integrates with a separately provisioned, exact-version gVisor binary but does not claim a hardware VM or confidential-computing boundary. The supplied Compose service has host-equivalent **outer-container** authority even though jobs use separate runsc workloads, so it is appropriate only inside a dedicated x86_64 Linux VM. For the exact assumptions and invariants, read [docs/security-boundary.md](docs/security-boundary.md).

## Security invariants

Production deployments must:

1. run gVisor or the namespace fallback only on supported x86_64 Linux and validate the exact host with their non-skipping gates;
2. pin the reviewed `runsc`, private-rootfs manifest digest, and generated OCI configuration evidence when using gVisor;
3. use scoped indexed credentials or strict RFC 9068 JWTs; if legacy keys remain, use a separate high-entropy key per tenant;
4. provision an owner-only Ed25519 attestation key, distribute its public key out of band, and treat `ROOKHOLD_ATTESTATION_MODE=off` as an explicit loss of signed evidence;
5. keep API traffic private or behind TLS;
6. use a dedicated non-symlink `ROOKHOLD_JOBS_ROOT`;
7. provide a trusted private `ROOKHOLD_ROOTFS` and never use host `/`;
8. keep namespace seccomp enabled and job networking denied in every isolated provider;
9. keep the database, backups, signing/identity secrets, outer filesystem, and sockets outside the job rootfs;
10. set measured tenant/global storage, aggregate memory, queue, and disk-reserve policies;
11. monitor authenticated status, metrics, receipts, and signed canaries—not only `/healthz`.

For every deployment mode, tenant ownership must be checked before job detail, result, event, stream, cancellation, attestation, or result-artifact access. Exported signed evidence must also bind the authoritative durable tenant in both its predicate and exact result; route authorization alone is not a portable identity claim. Isolated providers must fail closed instead of falling back to a subprocess, job networking must remain denied, resource/output/storage policies must remain bounded, and the job rootfs must never include Rookhold's database, credentials, keys, sockets, or outer host root. Development subprocess mode retains host networking, enforces only wall time among requested execution controls, and must report that posture. Per-job isolation claims must come from the executor's observed ready boundary; contradictory terminal provenance is an error. A status, receipt, or signature that reports a stronger policy or different result than was actually retained is a security defect.

Setting `ROOKHOLD_UNSAFE_ALLOW_NAIVE=true` is an acknowledgement, not a mitigation. It must never be used to process mutually untrusted code.

## Data sensitivity

The database and backups can contain submitted source, execution metadata, stdout/stderr, violations, tenant identifiers, hashes, receipts, exact result artifacts, and signed envelopes. Hashes may still reveal low-entropy inputs by guessing. Protect database files, WAL/SHM companions, logs, backups, exports, credential peppers, and private signing keys as customer/security data.

Event chains and receipt SHA-256 values are server-verifiable integrity metadata. DSSE envelopes authenticate the exact result/receipt assertion to an operator-pinned Ed25519 key, but remain neither a WORM guarantee nor trusted-hardware proof. A database administrator can delete evidence or replace both data and an unpinned key; key distribution, rotation history, backups, and external anchoring remain operator responsibilities.

## Reportable findings and severity context

Please report vulnerabilities that let an API client cross tenant authorization, execute with more host authority than the reported posture, escape the private rootfs or namespace boundary, enable job network access, bypass effective resource/output limits, expose credentials or customer evidence, interfere with another tenant's job, or cause evidence to be silently misattributed or reported as complete when it is not.

Host or cross-tenant code execution and secret disclosure are the highest-impact cases. Authorization bypass, a reliable boundary escape, remotely reachable host resource exhaustion within normal request limits, or evidence forgery by a non-administrator are also treated as serious. Unsafe defaults, posture misreporting, and credential leakage through documented workflows remain reportable even when no full escape has been demonstrated.

## Out of scope for a boundary claim

- kernel zero-days, speculative-execution leaks, and other side channels
- vulnerabilities in the separately reviewed gVisor/runtime build itself
- compromise of the trusted server account, VM, rootfs supply chain, or database
- interpreter/runtime vulnerabilities within the shared-kernel boundary
- availability under traffic beyond configured host/tenant capacity
- secrets deliberately placed in submitted code, stdin, environment, or rootfs

These issues may still be worth reporting if Rookhold makes exploitation materially easier or its documentation/configuration creates an unsafe default.
