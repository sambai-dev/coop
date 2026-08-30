# Upgrading

Coop uses semantic versioning while the API is pre-1.0: minor releases may contain intentional contract changes. Read [CHANGELOG.md](../CHANGELOG.md), compare `/openapi.json`, and test SDKs before upgrading production.

## Standard procedure

1. Drain new submissions and allow or cancel active jobs.
2. Take and verify an online or offline SQLite backup.
3. Record the current binary/image and private-rootfs digests.
4. Run the new version against a copy of the database.
5. Allow the transactional schema migration to complete; inspect its logs and authenticated `/v1/status`.
6. Exercise submit, stream, result, cancel, retention, and all language canaries.
7. Run the hostile suite on the target kernel/deployment shape.
8. Deploy, watch errors and terminal outcomes, then retain the rollback artifacts.

Do not downgrade a database after a new version has migrated it unless the release notes explicitly describe that path. Restore the pre-upgrade backup instead.

v0.4 records schema version 4 in both migration history and SQLite
`user_version`, refuses newer or physically downgraded/partial schemas, and
validates row types, UTF-8, digests, accounting ledgers, triggers, indexes, and
foreign keys before committing. Legacy jobs/events are preserved; old events
remain explicitly unverifiable rather than receiving fabricated hashes.

## From v0.1.x to v0.2.0

v0.2 tightens the security contract and should be treated as a security-sensitive upgrade:

- production namespace execution is Linux x86_64-only, requires `COOP_ROOTFS`, and never falls back to host `/`;
- `COOP_JOBS_ROOT` must be a dedicated absolute non-symlink path;
- production rejects blank, weak, and public development API keys;
- production cannot disable seccomp;
- an explicit production subprocess backend requires `COOP_UNSAFE_ALLOW_NAIVE=true` and remains unsafe;
- output is byte-bounded as well as record-bounded;
- terminal receipts and event-chain metadata are new; legacy records may be marked incomplete or unverifiable;
- requested and effective policy are distinct: `effective_spec` uses nullable
  per-control values, and policy/capability/receipt objects expose explicit
  `limit_enforcement`, `bootstrap_ready`, and `isolated` evidence;
- old receipts without the executor readiness marker remain readable but their
  runtime posture and effective policy are projected as unknown rather than
  inferred from the current server configuration;
- development-mode language capability is host-derived at startup; a missing
  Python, Node.js, or Bash runtime is omitted and submissions for it return
  `422 runtime_unavailable`;
- Docker builds include a separate private rootfs and the Compose posture is explicitly dedicated-VM-only.

Do not reuse a v0.1 container/root filesystem and simply change the binary. Build the v0.2 image or construct and validate a private rootfs first.

The v0.1 release made containment and audit claims that v0.2 deliberately narrows. Do not expose a v0.1 server to hostile tenants.

## From v0.2.x to v0.3.0

v0.3 keeps the v0.2 HTTP API, database schema, and Linux x86_64 execution
boundary. Stop the old service, back up SQLite and deployment configuration,
install the matching v0.3 `coop` and `coop-sandbox-init`, rebuild the private
rootfs/image from the v0.3 source, and run `scripts/verify-production.py`
before restoring traffic.

The Python wheel now installs `coop-mcp`; existing SDK imports remain
compatible. Agent operators should give each harness a separate tenant key,
set `COOP_MCP_REQUIRE_ISOLATION=true`, restrict its language allowlist, and
remove alternate execution tools when Coop must be mandatory rather than an
optional tool.

## From v0.3.x to v0.4.0

Treat v0.4 as a security- and deployment-sensitive upgrade:

1. drain v0.3, back up SQLite plus WAL/SHM consistently, and keep the complete
   v0.3 binary/image/rootfs/config rollback set;
2. test v0.4 against a copy—the schema-v3-to-v4 migration adds admitted-memory
   and accounting integrity plus attestation/outbox tables and is forward-only;
3. install Rust 1.98-built `coop`, `coop-verify`, `coop-sandbox-init`, and
   `coop-oci-init` from the same release;
4. rebuild the private rootfs and manifest, provision the exact reviewed
   `runsc`, set its manifest SHA-256, and run the real gVisor gate;
5. generate an owner-only Ed25519 key, retain its derived public key through an
   authenticated operator channel, and set `COOP_ATTESTATION_MODE=sign` plus
   `COOP_ATTESTATION_KEY_FILE`. Production refuses an implicit unsigned start;
   `off` must be explicit;
6. migrate legacy keys to the indexed credential file/pepper or configure the
   strict JWT issuer/audience/JWKS/tenant mapping and required scopes;
7. set tenant/global queue, memory, retained-byte, and disk-reserve budgets
   from measured host capacity;
8. update MCP policy from the legacy boolean to the exact
   `COOP_MCP_MINIMUM_ISOLATION` class and enable Tasks only where the host
   negotiates the 2026 extension;
9. run `scripts/verify-production.py` with the explicit public-key pin and the
   packaged `coop-verify` binary/image, then run the real Python/MCP adapter
   verifier and all language canaries before restoring traffic. The production
   script passes the exact signed-artifact bytes to the offline verifier and
   does not trust the server key endpoint.

Do not run a v0.3 binary against a migrated schema-v4 database. Restore the
pre-upgrade backup for rollback. Signing is durable but eventual, so consumers
requiring portable evidence must wait until `attestation.available` is true
and must pin the public key outside the signer API.
