# Operations, backup, and recovery

## Readiness

`GET /healthz` answers liveness only. It intentionally does not prove that the configured interpreter, cgroup controller, private rootfs, or event store can complete a job.

A readiness check should:

1. call authenticated `/v1/status` and compare the sandbox/backend posture to policy;
2. submit a small canary under a dedicated tenant;
3. wait through `/result`;
4. verify the terminal status, output, receipt/evidence completeness,
   `bootstrap_ready`, isolation facts, network posture, and every expected
   `limit_enforcement` flag;
5. alert if latency or queue depth exceeds the deployment budget.

Never route untrusted production traffic to a server reporting the plain subprocess backend. Its only effective resource control is wall time; requested CPU, memory, process, and file limits are not enforced.

## Signals and shutdown

Drain the reverse proxy before stopping Coop. On SIGTERM/Ctrl-C, Coop stops accepting HTTP work, requests cancellation of active executions, and gives workers up to 30 seconds to finalize. Accepted jobs that are still queued remain durable and are re-admitted on startup. A job that had reached `running` cannot be resumed after a process/host failure; boot recovery finalizes it as `error` with an interruption record. Inspect those receipts before deleting anything.

Compose and the systemd template allow a 45-second stop grace period. Keep the service-manager timeout longer than Coop's 30-second worker grace so HTTP draining and final persistence have headroom. If the worker grace expires, the lease drop synchronously requests a whole-cgroup kill, waits up to two seconds for `populated 0`, and removes the leaf before returning. A hard process or host kill can interrupt that bounded cleanup and SQLite checkpointing; verify no populated or stale Coop cgroup remains before restart, and let boot recovery finalize interrupted jobs.

## Monitoring

Scrape authenticated `/v1/metrics` only from the operator network. Metric names and labels are operational signals, not durable billing records. Combine them with structured logs and alerts for:

- queue rejection and sustained queueing
- sandbox bootstrap or rootfs validation failures
- cgroup cleanup failures or leaked descendants
- output truncation and policy violations
- timeout, OOM, cancellation, and internal-error rates
- SQLite busy, I/O, migration, or disk-capacity errors
- unexpected restarts and boot-recovered jobs
- rate-limit pressure by tenant
- response-capacity pressure, incomplete-response retries, and HTTP write-progress timeouts

Keep host metrics for memory, CPU, cgroup count, disk latency/free space, inode use, and kernel audit events. The server process itself is outside each job's cgroup, so host-level capacity matters.

## Capacity validation

From a source checkout, use a dedicated benchmark tenant against the exact VM/rootfs shape:

```bash
python scripts/bench.py \
  --url http://127.0.0.1:7300 \
  --key BENCHMARK_TENANT_KEY \
  --jobs 50 \
  --concurrency 4 \
  --wait-seconds 60
```

The default run submits two warmups plus 50 measured jobs and normally makes 104 authenticated requests, below the default 120-request tenant budget. The benchmark honors `Retry-After` and uses one server-side result wait per job. For larger trials, schedule a controlled window and set an intentional rate budget; do not disable admission controls on a live tenant. Record the Coop/image/rootfs digest, VM shape, worker/concurrency settings, language versions, and outcome mix with the latency table.

## Retention

`COOP_RETENTION_HOURS` controls deletion of terminal jobs and their events. A value of `0` disables automatic deletion. Retention is not archival: copy required evidence to your controlled archive before its deadline.

Capacity planning must include the main SQLite file plus `-wal` and `-shm` companions, logs, backups, and temporary job staging. Output caps bound each job but do not bound total retained volume.

## Online backup

Use SQLite's online backup mechanism rather than copying only the main file while Coop is running:

```bash
sqlite3 /var/lib/coop/coop.db \
  ".timeout 10000" \
  ".backup '/secure-backups/coop-$(date -u +%Y%m%dT%H%M%SZ).db'"
```

Run `PRAGMA integrity_check;` against the backup, encrypt it, record a checksum, and transfer it to access-controlled storage. Database contents include submitted code, stdin-derived behavior, stdout/stderr, tenant identifiers, and evidence metadata. Treat backups as sensitive.

Inside Compose, either install/use `sqlite3` on the host against a carefully exposed backup path, or stop the service cleanly and copy the named volume. Do not add broad host mounts to the Coop container merely to simplify backup.

## Offline backup

For a filesystem-level copy:

1. drain and stop Coop;
2. verify no `coop` process has the database open;
3. copy the database and any WAL/SHM files together, or checkpoint first;
4. copy configuration metadata, the Coop binary/image digest, private-rootfs digest, and release version;
5. restart and run a canary.

## Restore test

Restores should be rehearsed on an isolated host:

1. verify backup checksum and decrypt to a private path;
2. start the same Coop version against a copy of the restored database;
3. run `PRAGMA integrity_check;`;
4. verify several jobs, ordered event chains, and receipts;
5. upgrade the copy if required and repeat verification;
6. destroy the rehearsal copy securely.

Never point two live Coop instances at the same SQLite file. Coop takes an adjacent process lock and rejects symlinked or hard-linked database aliases, but that guard is not a substitute for operator discipline around bind mounts or network filesystems. Before replacing production data, stop the service and preserve the failed/current database for forensics.

## Rootfs maintenance

Treat the private rootfs as an immutable release artifact. Build a new tree, patch interpreters and libraries, compute its manifest/digest, run all language canaries and hostile tests, then switch `COOP_ROOTFS` during a controlled restart. Do not mutate the live tree while jobs are running.
