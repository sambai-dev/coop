# Security policy

## Reporting a vulnerability

Email `sambai.codes@gmail.com` with a subject beginning `[coop security]`. If GitHub private vulnerability reporting is enabled for the repository, the **Security** tab's **Report a vulnerability** form is also acceptable. Do not include exploit details, tenant data, credentials, or unredacted execution evidence in a public issue.

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
| tagged `0.3.x` releases | Supported |
| tagged `0.2.x` releases | Unsupported; upgrade to 0.3.x |
| `0.1.x` | Unsupported; do not expose to hostile tenants |
| older versions | Unsupported |

`main` can describe changes not yet present in a tagged artifact. A source checkout is not a release artifact; use a commit digest when reporting it.

## Boundary summary

Coop v0.3 has two materially different execution modes:

- `namespaces`: a shared-kernel Linux x86_64 defense-in-depth backend requiring a private rootfs, cgroup v2, rlimits, the x86_64 seccomp policy, checked privilege drop, and isolated namespaces. Run it only on a dedicated x86_64 VM.
- `off`: a plain subprocess for same-trust development. It is not a sandbox,
  submitted code has the service account's authority, and only wall time (not
  requested CPU, memory, process-count, or file-size limits) is enforced.

Namespace containment is unsupported on macOS, Windows, and non-x86_64 Linux; those platforms can use only the unisolated development subprocess backend. Coop does not bundle gVisor, Kata, Firecracker, or another VM boundary. The supplied privileged Compose service has host-equivalent container authority and is appropriate only inside a dedicated x86_64 Linux VM. For the exact assumptions and invariants, read [docs/security-boundary.md](docs/security-boundary.md).

## Security invariants

Production deployments must:

1. run the namespace backend only on supported x86_64 Linux;
2. use a separate high-entropy key for each tenant;
3. keep API traffic private or behind TLS;
4. use a dedicated non-symlink `COOP_JOBS_ROOT`;
5. provide a trusted private `COOP_ROOTFS` and never use host `/`;
6. keep seccomp enabled;
7. keep the database, backups, outer filesystem, sockets, and credentials outside the job rootfs;
8. validate the exact host with the hostile suite;
9. monitor authenticated status and canary receipts, not only `/healthz`.

For every deployment mode, tenant ownership must be checked before job detail, result, event, stream, or cancellation access. Namespace mode must fail closed instead of falling back to a subprocess, namespace job networking must remain denied, resource/output policies must remain server-clamped and bounded, and the job rootfs must never include the Coop database, credentials, sockets, or outer host root. Development subprocess mode retains host networking, enforces only wall time among the requested resource controls, and must report that posture. Per-job namespace claims must come from the executor's observed ready boundary rather than configuration or a durable `running` state. A status or receipt that reports a stronger backend or policy than was actually enforced is a security defect.

Setting `COOP_UNSAFE_ALLOW_NAIVE=true` is an acknowledgement, not a mitigation. It must never be used to process mutually untrusted code.

## Data sensitivity

The database and backups can contain submitted source, execution metadata, stdout/stderr, violations, tenant identifiers, hashes, and receipts. Hashes may still reveal low-entropy inputs by guessing. Protect database files, WAL/SHM companions, logs, backups, and receipt exports as customer data.

Event chains and receipt SHA-256 values are server-verifiable integrity metadata. They are not digital signatures, a WORM guarantee, or proof against an administrator who can rewrite the database and recompute hashes.

## Reportable findings and severity context

Please report vulnerabilities that let an API client cross tenant authorization, execute with more host authority than the reported posture, escape the private rootfs or namespace boundary, enable job network access, bypass effective resource/output limits, expose credentials or customer evidence, interfere with another tenant's job, or cause evidence to be silently misattributed or reported as complete when it is not.

Host or cross-tenant code execution and secret disclosure are the highest-impact cases. Authorization bypass, a reliable boundary escape, remotely reachable host resource exhaustion within normal request limits, or evidence forgery by a non-administrator are also treated as serious. Unsafe defaults, posture misreporting, and credential leakage through documented workflows remain reportable even when no full escape has been demonstrated.

## Out of scope for a boundary claim

- kernel zero-days, speculative-execution leaks, and other side channels
- compromise of the trusted server account, VM, rootfs supply chain, or database
- interpreter/runtime vulnerabilities within the shared-kernel boundary
- availability under traffic beyond configured host/tenant capacity
- secrets deliberately placed in submitted code, stdin, environment, or rootfs

These issues may still be worth reporting if Coop makes exploitation materially easier or its documentation/configuration creates an unsafe default.
