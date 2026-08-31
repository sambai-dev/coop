# Troubleshooting

## Server refuses to start in production

Read the first error line; startup checks are intentionally fail closed.

- `ROOKHOLD_API_KEYS`: use `tenant:key`, a nonblank tenant, and a random key of at least the enforced minimum length.
- attestation rejected: production needs an absolute owner-only canonical
  Ed25519 PKCS#8 `ROOKHOLD_ATTESTATION_KEY_FILE`, unless
  `ROOKHOLD_ATTESTATION_MODE=off` was an explicit unsigned-evidence decision.
- gVisor rejected: verify the absolute `runsc` version/SHA-256, executable
  ownership, `vm.max_map_count`, rootfs manifest, its configured SHA-256, and
  matching `rookhold-oci-init` before retrying.
- namespace unavailable: confirm x86_64 Linux 5.14+, effective UID 0, unified cgroup v2, `cgroup.kill`, recursive `mount_setattr`, and a writable delegated cgroup subtree. The current backend does not support non-root delegation; macOS, Windows, and non-x86_64 Linux are unsupported for containment.
- helper rejected: install `rookhold-sandbox-init` from the exact same build as `rookhold`, set `ROOKHOLD_SANDBOX_HELPER`, and keep it root-owned/non-writable by jobs.
- rootfs rejected: set `ROOKHOLD_ROOTFS` to a dedicated absolute directory; never `/`, the jobs root, or a symlink.
- jobs root rejected: choose a dedicated absolute child such as `/var/lib/rookhold/jobs`, not `/tmp`, `/var`, a home root, or a symlinked path.
- database/instance lock rejected: use one dedicated regular `ROOKHOLD_DB` file. Symlinks and hard-linked SQLite aliases are intentionally unsupported, and a second Rookhold process cannot own the same database.
- seccomp rejected: production does not permit `ROOKHOLD_SECCOMP=off`.
- subprocess rejected: do not use it for untrusted work; the explicit acknowledgement is `ROOKHOLD_UNSAFE_ALLOW_NAIVE=true`.

For Compose, do not point the server directly at the host-owned private key or
`runsc`. File-backed secrets and bind mounts preserve the host UID, while the
production readers require root-owned in-container files. The supplied image
entrypoint stages both inputs into root-owned tmpfs paths and the bootstrap
validates that mapping through the exact built image. If the staging preflight
fails under rootless Docker or `userns-remap`, use the supported rootful
dedicated-VM posture; do not loosen file modes or ownership checks.

## `/healthz` is green but jobs fail

Liveness only proves that the HTTP process responds. Check authenticated `/v1/status`, startup logs, disk space, interpreter paths inside the private rootfs, cgroup controllers, and a canary receipt.

An interpreter override must be meaningful both to the outer launcher and after the rootfs pivot. If `/usr/local/bin/python3` exists only on the host, either place the approved binary and dependencies at the same path in the rootfs or use a different override.

## `503` on submit

The admission queue, global response/body capacity, storage service, or worker
service is unavailable. Back off with jitter and inspect the structured error
code, tenant/global queue leases, weighted memory, logical storage, disk
reserve, worker health, and long-running jobs. Increasing workers without host
capacity can make isolation and SQLite contention worse.

## `429` during wait or benchmark

Use `/result?wait_seconds=N`; do not poll `/v1/jobs/{id}` every few milliseconds. Honor `Retry-After`. The committed benchmark uses one result wait per job and backs off on rate limits.

## WebSocket connects but output is missing or duplicated

Persisted history is sent before live events. Deduplicate by job ID and event sequence, not by line text. Make sure the proxy supports WebSocket upgrades and has an idle timeout longer than the job budget. Rookhold intentionally expires every connection after 10 minutes; mint a new stream ticket and resume from the last accepted sequence. On credential rotation, close the old socket before opening a new tenant session.

## Job reports output truncation

Truncation protects server memory and storage. Read the receipt's observed byte counts and hashes, reduce verbosity, write a bounded summary, or split the work. Raising the cap requires a reviewed server policy change; it is not a per-request escape hatch.

## Jobs cannot access the network

This is expected in the Linux x86_64 gVisor and namespace providers.
`allow_network: true` is not supported. Move required fetching into a trusted
adapter, validate the data, and pass bounded input to the job. If status reports
`networking: "host"`, the server is using the unisolated development subprocess
and must not run untrusted code.

## Terminal job has no signed attestation

Signing is durable but asynchronous. Retry job detail briefly and require
`attestation.available: true` before downloading. If it stays false, inspect
attestation retry/key/storage warnings and the signer capability. Do not call
an unavailable envelope “verified.” `ROOKHOLD_ATTESTATION_MODE=off` intentionally
produces no signature; after enabling a key, only retained terminal jobs can be
backfilled. Preserve historical public keys when rotating.

## Production verification rejects the attestation

`scripts/verify-production.py` requires an absolute
`ROOKHOLD_VERIFY_PUBLIC_KEY_FILE` obtained independently of the server. For the
Compose bootstrap, use `.coop-runtime/attestation-public-key.pem` and set
`ROOKHOLD_VERIFY_CONTAINER_IMAGE` to the immutable built image ID. For a binary
installation, set `ROOKHOLD_VERIFY_BIN` to the packaged verifier. A missing pin,
nonzero verifier exit, wrong key, modified envelope/result byte, unexpected
subject, or incomplete authenticated event chain fails the gate. Do not fix a
failure by downloading `/v1/attestation/public-key` from the deployment being
checked; that endpoint is discovery material, not a trust anchor.

## SQLite is busy or disk is full

Stop new submissions, preserve logs, and check the database, WAL/SHM files, backups, and job staging. Do not delete WAL/SHM files from a live database. Use SQLite's online backup/checkpoint tools or stop Rookhold before filesystem manipulation. Review retention only after preserving required evidence.

## Cgroup directories remain after jobs

Treat this as a containment fault. Stop admission, capture logs and `cgroup.events`, identify live descendants, and isolate the VM. Do not recursively delete a populated cgroup directory. Normal terminal paths use `cgroup.kill`, wait for `populated 0`, and remove the job cgroup. A panic or forced worker abort runs the same cleanup synchronously with a two-second bound; only a cleanup error or hard process/host kill should leave the leaf behind. Verify it is empty and reconcile it before restoring admission.

## Hostile tests appear to pass instantly

The tests are intentionally ignored by ordinary test runs and require x86_64 Linux, root, and cgroup v2. Run:

```bash
sudo env \
  ROOKHOLD_ROOTFS=/opt/rookhold/rootfs \
  ROOKHOLD_SANDBOX_HELPER=/usr/local/bin/rookhold-sandbox-init \
  cargo test --locked -p coop-server --test hostile -- --ignored --nocapture
```

Confirm that exactly 18 tests ran, prerequisites were detected, and none reported `SKIP:`. Release CI constructs a private rootfs and performs a separate preflight so an environmental skip cannot produce a green containment gate.
