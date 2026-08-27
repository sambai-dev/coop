# Troubleshooting

## Server refuses to start in production

Read the first error line; startup checks are intentionally fail closed.

- `COOP_API_KEYS`: use `tenant:key`, a nonblank tenant, and a random key of at least the enforced minimum length.
- namespace unavailable: confirm x86_64 Linux 5.14+, effective UID 0, unified cgroup v2, `cgroup.kill`, recursive `mount_setattr`, and a writable delegated cgroup subtree. The current backend does not support non-root delegation; macOS, Windows, and non-x86_64 Linux are unsupported for containment.
- helper rejected: install `coop-sandbox-init` from the exact same build as `coop`, set `COOP_SANDBOX_HELPER`, and keep it root-owned/non-writable by jobs.
- rootfs rejected: set `COOP_ROOTFS` to a dedicated absolute directory; never `/`, the jobs root, or a symlink.
- jobs root rejected: choose a dedicated absolute child such as `/var/lib/coop/jobs`, not `/tmp`, `/var`, a home root, or a symlinked path.
- database/instance lock rejected: use one dedicated regular `COOP_DB` file. Symlinks and hard-linked SQLite aliases are intentionally unsupported, and a second Coop process cannot own the same database.
- seccomp rejected: production does not permit `COOP_SECCOMP=off`.
- subprocess rejected: do not use it for untrusted work; the explicit acknowledgement is `COOP_UNSAFE_ALLOW_NAIVE=true`.

## `/healthz` is green but jobs fail

Liveness only proves that the HTTP process responds. Check authenticated `/v1/status`, startup logs, disk space, interpreter paths inside the private rootfs, cgroup controllers, and a canary receipt.

An interpreter override must be meaningful both to the outer launcher and after the rootfs pivot. If `/usr/local/bin/python3` exists only on the host, either place the approved binary and dependencies at the same path in the rootfs or use a different override.

## `503` on submit

The admission queue or worker service is unavailable. Back off with jitter and inspect queue pressure, worker health, tenant concurrency, and long-running jobs. Increasing workers without increasing host capacity can make isolation and SQLite contention worse.

## `429` during wait or benchmark

Use `/result?wait_seconds=N`; do not poll `/v1/jobs/{id}` every few milliseconds. Honor `Retry-After`. The committed benchmark uses one result wait per job and backs off on rate limits.

## WebSocket connects but output is missing or duplicated

Persisted history is sent before live events. Deduplicate by job ID and event sequence, not by line text. Make sure the proxy supports WebSocket upgrades and has an idle timeout longer than the job budget. Coop intentionally expires every connection after 10 minutes; mint a new stream ticket and resume from the last accepted sequence. On credential rotation, close the old socket before opening a new tenant session.

## Job reports output truncation

Truncation protects server memory and storage. Read the receipt's observed byte counts and hashes, reduce verbosity, write a bounded summary, or split the work. Raising the cap requires a reviewed server policy change; it is not a per-request escape hatch.

## Jobs cannot access the network

This is expected in the supported Linux x86_64 namespace backend. `allow_network: true` is not supported. Move required fetching into a trusted adapter, validate the data, and pass bounded input to the job. If status reports `networking: "host"`, the server is using the unisolated development subprocess backend; it has host egress and must not run untrusted code.

## SQLite is busy or disk is full

Stop new submissions, preserve logs, and check the database, WAL/SHM files, backups, and job staging. Do not delete WAL/SHM files from a live database. Use SQLite's online backup/checkpoint tools or stop Coop before filesystem manipulation. Review retention only after preserving required evidence.

## Cgroup directories remain after jobs

Treat this as a containment fault. Stop admission, capture logs and `cgroup.events`, identify live descendants, and isolate the VM. Do not recursively delete a populated cgroup directory. Normal terminal paths use `cgroup.kill`, wait for `populated 0`, and remove the job cgroup. A panic or forced worker abort runs the same cleanup synchronously with a two-second bound; only a cleanup error or hard process/host kill should leave the leaf behind. Verify it is empty and reconcile it before restoring admission.

## Hostile tests appear to pass instantly

The tests are intentionally ignored by ordinary test runs and require x86_64 Linux, root, and cgroup v2. Run:

```bash
sudo env \
  COOP_ROOTFS=/opt/coop/rootfs \
  COOP_SANDBOX_HELPER=/usr/local/bin/coop-sandbox-init \
  cargo test --locked -p coop-server --test hostile -- --ignored --nocapture
```

Confirm that exactly 18 tests ran, prerequisites were detected, and none reported `SKIP:`. Release CI constructs a private rootfs and performs a separate preflight so an environmental skip cannot produce a green containment gate.
