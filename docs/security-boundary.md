# Security boundary and trust tiers

Rookhold runs code supplied by API clients. That makes containment configuration part of the product contract, not an optional deployment detail.

## Trust tiers

| Tier | Intended use | Boundary | Current posture |
|---|---|---|---|
| gVisor application kernel | Determined hostile multi-tenant code | Reviewed `runsc` OCI workload and private rootfs per job | Guarded production default; runtime/rootfs/config digests retained as evidence |
| Hardware or confidential VM | Determined hostile multi-tenant code | MicroVM/VM per workload | API class reserved; provider not bundled |
| Linux x86_64 namespace backend | Untrusted or accidentally dangerous agent code on a dedicated x86_64 VM | Private rootfs, Linux namespaces, cgroup v2, rlimits, x86_64 seccomp policy, privilege drop | In tree; shared-kernel defense in depth |
| Plain subprocess | Local development and same-trust code only | Process group and wall-time supervision | Unisolated; never a sandbox |

The namespace backend is not equivalent to a VM. A kernel vulnerability, side channel, interpreter vulnerability, or privileged-container escape can cross it. It is supported only on Linux x86_64. The reviewed gVisor provider adds a per-job application-kernel boundary and records its exact runtime, rootfs, and OCI configuration digests. macOS, Windows, and other Linux architectures use the plain development subprocess backend unless a separately reviewed provider is configured, and therefore provide no default containment. That backend supervises cancellation, bounded output, and wall time, but does not enforce the requested CPU, memory, process-count, or file-size controls. Those controls remain null/false in its effective policy and receipts.

## gVisor-provider invariants

A gVisor job is expected to fail closed unless all of these are true:

1. `runsc` is an absolute, non-writable executable matching the reviewed version and SHA-256.
2. the private rootfs and its content-complete manifest validate before workload creation; the OCI root is read-only;
3. each job receives a unique runtime ID, OCI bundle, payload directory, cgroup, private `/tmp`/`/var/tmp`, and non-root uid/gid;
4. the OCI configuration drops capabilities, sets `noNewPrivileges`, denies networking, applies rlimits/cgroup controls, and binds its canonical SHA-256 into provenance;
5. the dedicated `rookhold-oci-init` confirms `/.coop-rootfs.manifest`, `/proc/gvisor/kernel_is_gvisor`, and Rookhold's marker before launching user code;
6. a nonce-bound pass-fd ready frame is observed before any gVisor isolation class or effective control is reported;
7. normal exit, cancellation, timeout, bootstrap failure, server crash, and provider switching all converge through runsc wait/kill/delete plus cgroup drain/removal;
8. terminal evidence records the exact runtime, rootfs-manifest, and OCI-config digests, and the scheduler rejects contradictory observed isolation.

The real release gate executes these paths with the pinned runtime. It does not
generalize to arbitrary gVisor builds, kernels, KVM configuration, or hardware
VM/confidential-computing claims.

## Namespace-backend invariants

A production namespace job is expected to fail closed unless all of these are true:

1. Rookhold runs on x86_64 Linux 5.14 or newer with cgroup v2, `cgroup.kill`, recursive `mount_setattr`, and the required privileges.
2. `ROOKHOLD_ROOTFS` names a trusted, purpose-built, absolute directory.
3. The rootfs is not `/`, does not traverse symlinks, and is not writable by job credentials.
4. The job receives its own mount, PID, network, IPC, and UTS namespaces.
5. The process that follows PID-namespace creation becomes namespace PID 1 and reaps descendants.
6. The rootfs is pivoted before the interpreter runs; the old root is detached.
7. `/proc`, temporary storage, stdin, and source staging are job-private.
8. cgroup memory, process, CPU, and cleanup controls are installed before execution.
9. supplementary groups are cleared and uid/gid dropping is checked.
10. `no_new_privs` and the x86_64 seccomp policy are applied.
11. normal completion, timeout, cancellation, and bootstrap-error paths kill the cgroup, wait for it to empty, and record the outcome. A panic or forced worker abort still issues a synchronous cgroup kill, but its wait/remove cleanup is best-effort and may need operator reconciliation.
12. stdout and stderr cannot allocate or queue unbounded data in the server.

The authenticated `/v1/status` response and startup logs expose the selected backend. Treat an unexpected status as a failed deployment, even if `/healthz` is green.

## Filesystem model

The private rootfs contains only the interpreters and libraries intended for jobs. It has an empty `/.pivot_old` plus `/proc`, `/dev`, `/tmp`, `/tmp/home`, and `/work` mount points; bootstrap replaces the mutable paths with job-private mounts. The Docker image builds this tree at `/opt/rookhold/rootfs`; source deployments must create an equivalent tree and set `ROOKHOLD_ROOTFS` explicitly.

Do not put secrets, the Rookhold database, host sockets, cloud credentials, SSH material, or a Docker socket in the rootfs. Do not bind the server data volume into it. Interpreter overrides must resolve to a path present inside the rootfs.

`ROOKHOLD_JOBS_ROOT` is server-side staging, not a shared workspace. It must be a dedicated absolute non-symlink directory. Rookhold rejects broad locations because applying owner-only permissions to `/`, `/var`, a home root, or a redirected path could damage the host.

## Network model

The Linux x86_64 namespace and gVisor providers deny job network access. A submitted `allow_network: true` is rejected; it is not an egress grant. The unisolated development subprocess backend cannot enforce this boundary and retains the service account's host networking, reported as `networking: "host"`. The server itself may also reach the network, so firewall its egress and metadata-service access at the VM boundary.

A future egress feature should be a host-side policy proxy with explicit destination, DNS-rebinding, method, and credential-injection rules—not a general network namespace interface.

## Authentication and transport

- Prefer indexed peppered-HMAC credentials with explicit principal, tenant,
  scopes, expiry, and revocation, or strict RFC 9068 JWTs with pinned
  issuer/audience/JWKS/tenant mapping. Legacy tenant keys are migration-only.
- Grant only the required `jobs:submit`, `jobs:read`, `jobs:cancel`,
  `service:read`, and `metrics:read` scopes; use a distinct global scrape token.
- Keep Rookhold on a private network or loopback behind a TLS-terminating proxy.
- Do not put API keys in URLs. Query strings are commonly logged by browsers and proxies.
- Prefer the `Authorization` header for WebSocket clients that support it.
- Clear dashboard state and close streams when rotating credentials.

Credential files contain indexed HMAC digests rather than plaintext secrets;
the pepper and signing private key remain separate protected files. JWT tokens
are verified against a bounded cached JWKS and never persisted. Browser
credentials remain memory-only and legacy local/session storage is purged.

## Evidence semantics

The SQLite event history supports incident reconstruction: ordered lifecycle,
output, truncation, violation, and terminal events. Canonical events link with
SHA-256 and the terminal receipt binds their head. Schema v4 additionally
stores a deterministic result artifact and an Ed25519 DSSE/in-toto envelope
through a durable signing outbox. Persistence is conditional on unchanged
receipt bytes; restart backfills missing retained signatures.

The signature proves possession of the configured key over the exact
statement/result digest. It is not execution truth, a WORM primitive,
transparency anchoring, or remote hardware attestation. `keyid` is only a hint,
and the current public-key endpoint is not an independent trust anchor. Pin
keys out of band and preserve old keys through rotation.

Configuration is not execution evidence. Namespace posture is attached only
after the executor observes the helper's workload-ready control frame. A
durable `running` transition or telemetry collected before that frame does not
prove that pivot-root, privilege drop, seccomp, or other containment became
active. Pre-ready failures therefore record `bootstrap_ready: false`, false
isolation facts, and no effective controls; interrupted executions whose
executor observation cannot be recovered omit that posture.

For higher-assurance retention, export database snapshots, exact artifacts,
envelopes, logs, and public-key history to access-controlled immutable storage.
Preserve configuration plus binary/runtime/rootfs/OCI digests alongside them.

## Deployment rules

- Use a dedicated x86_64 Linux VM with no unrelated workloads.
- Patch the kernel, container runtime, interpreters, and Rookhold promptly.
- Never mount `/var/run/docker.sock` or host credential directories.
- Keep the API loopback-only unless a private TLS proxy is present.
- Restrict access to the database, its WAL/SHM companions, backups, and rootfs.
- Alert on restarts, provider/bootstrap/digest failures, cgroup cleanup,
  attestation retries, output truncation, quota pressure, and violations.
- Run both the hostile namespace suite and real gVisor lifecycle gate on the
  exact kernel/deployment shape before admitting traffic.

The supplied Compose file uses `privileged: true` because the outer control
plane manages runsc, mounts, and cgroups. That grants host-equivalent container
authority even though each tenant job enters gVisor. Compose is therefore a
packaging convenience inside a dedicated VM, not a hardened Docker-host
control-plane boundary.
