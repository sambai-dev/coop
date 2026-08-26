# Security boundary and trust tiers

Coop runs code supplied by API clients. That makes containment configuration part of the product contract, not an optional deployment detail.

## Trust tiers

| Tier | Intended use | Boundary | v0.2 posture |
|---|---|---|---|
| External hardened runtime | Determined hostile multi-tenant code | gVisor/OCI, Kata, or microVM per workload | Recommended direction; not bundled or integrated in v0.2 |
| Linux x86_64 namespace backend | Untrusted or accidentally dangerous agent code on a dedicated x86_64 VM | Private rootfs, Linux namespaces, cgroup v2, rlimits, x86_64 seccomp policy, privilege drop | In tree; shared-kernel defense in depth |
| Plain subprocess | Local development and same-trust code only | Process group and wall-time supervision | Unisolated; never a sandbox |

The namespace backend is not equivalent to a VM. A kernel vulnerability, side channel, interpreter vulnerability, or privileged-container escape can cross it. It is supported only on Linux x86_64 in v0.2. macOS, Windows, and other Linux architectures use the plain development subprocess backend and provide no containment. That backend supervises cancellation, bounded output, and wall time, but does not enforce the requested CPU, memory, process-count, or file-size controls. Those controls remain null/false in its effective policy and receipts. For code controlled by mutually hostile tenants, use a per-workload hardened runtime or VM boundary once an integration is available.

## Namespace-backend invariants

A production namespace job is expected to fail closed unless all of these are true:

1. Coop runs on x86_64 Linux 5.14 or newer with cgroup v2, `cgroup.kill`, recursive `mount_setattr`, and the required privileges.
2. `COOP_ROOTFS` names a trusted, purpose-built, absolute directory.
3. The rootfs is not `/`, does not traverse symlinks, and is not writable by job credentials.
4. The job receives its own mount, PID, network, IPC, and UTS namespaces.
5. The process that follows PID-namespace creation becomes namespace PID 1 and reaps descendants.
6. The rootfs is pivoted before the interpreter runs; the old root is detached.
7. `/proc`, temporary storage, stdin, and source staging are job-private.
8. cgroup memory, process, CPU, and cleanup controls are installed before execution.
9. supplementary groups are cleared and uid/gid dropping is checked.
10. `no_new_privs` and the v0.2 x86_64 seccomp policy are applied.
11. normal completion, timeout, cancellation, and bootstrap-error paths kill the cgroup, wait for it to empty, and record the outcome. A panic or forced worker abort still issues a synchronous cgroup kill, but its wait/remove cleanup is best-effort and may need operator reconciliation.
12. stdout and stderr cannot allocate or queue unbounded data in the server.

The authenticated `/v1/status` response and startup logs expose the selected backend. Treat an unexpected status as a failed deployment, even if `/healthz` is green.

## Filesystem model

The private rootfs contains only the interpreters and libraries intended for jobs. It has an empty `/.pivot_old` plus `/proc`, `/dev`, `/tmp`, `/tmp/home`, and `/work` mount points; bootstrap replaces the mutable paths with job-private mounts. The Docker image builds this tree at `/opt/coop/rootfs`; source deployments must create an equivalent tree and set `COOP_ROOTFS` explicitly.

Do not put secrets, the Coop database, host sockets, cloud credentials, SSH material, or a Docker socket in the rootfs. Do not bind the server data volume into it. Interpreter overrides must resolve to a path present inside the rootfs.

`COOP_JOBS_ROOT` is server-side staging, not a shared workspace. It must be a dedicated absolute non-symlink directory. Coop rejects broad locations because applying owner-only permissions to `/`, `/var`, a home root, or a redirected path could damage the host.

## Network model

The supported Linux x86_64 namespace backend denies job network access. A submitted `allow_network: true` is rejected; it is not an egress grant in v0.2. The unisolated development subprocess backend cannot enforce this boundary and retains the service account's host networking, reported as `networking: "host"`. The server itself may also reach the network, so firewall its egress and metadata-service access at the VM boundary.

A future egress feature should be a host-side policy proxy with explicit destination, DNS-rebinding, method, and credential-injection rules—not a general network namespace interface.

## Authentication and transport

- Use a different high-entropy API key per tenant and workload class.
- Keep Coop on a private network or loopback behind a TLS-terminating proxy.
- Do not put API keys in URLs. Query strings are commonly logged by browsers and proxies.
- Prefer the `Authorization` header for WebSocket clients that support it.
- Clear dashboard state and close streams when rotating credentials.

API keys are stored in process configuration, not hashed in SQLite. Rotation currently requires updating configuration and restarting the service.

## Evidence semantics

The SQLite event history supports incident reconstruction: ordered lifecycle, output, truncation, violation, and terminal events. v0.2 links canonical events with SHA-256 and binds the terminal chain head into a receipt. The database is still mutable by the server account or anyone with database access; the chain is not signed, externally anchored, an append-only storage primitive, or remote attestation.

Configuration is not execution evidence. Namespace posture is attached only
after the executor observes the helper's workload-ready control frame. A
durable `running` transition or telemetry collected before that frame does not
prove that pivot-root, privilege drop, seccomp, or other containment became
active. Pre-ready failures therefore record `bootstrap_ready: false`, false
isolation facts, and no effective controls; interrupted executions whose
executor observation cannot be recovered omit that posture.

For higher-assurance audit retention, export database snapshots and logs to access-controlled immutable storage. Preserve the configuration, binary digest, private-rootfs digest, and host/runtime metadata alongside them.

## Deployment rules

- Use a dedicated x86_64 Linux VM with no unrelated workloads.
- Patch the kernel, container runtime, interpreters, and Coop promptly.
- Never mount `/var/run/docker.sock` or host credential directories.
- Keep the API loopback-only unless a private TLS proxy is present.
- Restrict access to the database, its WAL/SHM companions, backups, and rootfs.
- Alert on restarts, sandbox bootstrap failures, cgroup cleanup failures, output truncation, and repeated violations.
- Run the hostile suite on the exact kernel and deployment shape before admitting traffic.

The supplied Compose file uses `privileged: true` because the in-tree backend manages namespaces, mounts, and cgroups. That flag grants host-equivalent container authority. Compose is therefore only a packaging convenience inside a dedicated VM, not a hardened Docker-host boundary.
