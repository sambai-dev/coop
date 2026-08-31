# Upgrading

Rookhold uses semantic versioning while the API is pre-1.0: minor releases may contain intentional contract changes. Read [CHANGELOG.md](../CHANGELOG.md), compare `/openapi.json`, and test SDKs before upgrading production.

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

- production namespace execution is Linux x86_64-only, requires `ROOKHOLD_ROOTFS`, and never falls back to host `/`;
- `ROOKHOLD_JOBS_ROOT` must be a dedicated absolute non-symlink path;
- production rejects blank, weak, and public development API keys;
- production cannot disable seccomp;
- an explicit production subprocess backend requires `ROOKHOLD_UNSAFE_ALLOW_NAIVE=true` and remains unsafe;
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
install the matching v0.3 `coop` and `rookhold-sandbox-init`, rebuild the private
rootfs/image from the v0.3 source, and run `scripts/verify-production.py`
before restoring traffic.

The Python wheel now installs `rookhold-mcp`; existing SDK imports remain
compatible. Agent operators should give each harness a separate tenant key,
set `ROOKHOLD_MCP_REQUIRE_ISOLATION=true`, restrict its language allowlist, and
remove alternate execution tools when Rookhold must be mandatory rather than an
optional tool.

## From v0.3.x to v0.4.0

Treat v0.4 as a security- and deployment-sensitive upgrade:

1. drain v0.3, back up SQLite plus WAL/SHM consistently, and keep the complete
   v0.3 binary/image/rootfs/config rollback set;
2. test v0.4 against a copy—the schema-v3-to-v4 migration adds admitted-memory
   and accounting integrity plus attestation/outbox tables and is forward-only;
   migrated terminal receipts keep their exact v0.3 bytes and checksum, while
   first-time v0.4 attestations bind the authoritative `jobs.tenant` separately;
3. install Rust 1.98-built `coop`, `rookhold-verify`, `rookhold-sandbox-init`, and
   `rookhold-oci-init` from the same release;
4. rebuild the private rootfs and manifest, provision the exact reviewed
   `runsc`, set its manifest SHA-256, and run the real gVisor gate;
5. generate an owner-only Ed25519 key, retain its derived public key through an
   authenticated operator channel, and set `ROOKHOLD_ATTESTATION_MODE=sign` plus
   `ROOKHOLD_ATTESTATION_KEY_FILE`. Production refuses an implicit unsigned start;
   `off` must be explicit;
6. migrate legacy keys to the indexed credential file/pepper or configure the
   strict JWT issuer/audience/JWKS/tenant mapping and required scopes;
7. set tenant/global queue, memory, retained-byte, and disk-reserve budgets
   from measured host capacity;
8. update MCP policy from the legacy boolean to the exact
   `ROOKHOLD_MCP_MINIMUM_ISOLATION` class and enable Tasks only where the host
   negotiates the 2026 extension;
9. run `scripts/verify-production.py` with the explicit public-key pin and the
   packaged `rookhold-verify` binary/image, then run the real Python/MCP adapter
   verifier and all language canaries before restoring traffic. The production
   script passes the exact signed-artifact bytes to the offline verifier and
   does not trust the server key endpoint.

Do not run a v0.3 binary against a migrated schema-v4 database. Restore the
pre-upgrade backup for rollback. Signing is durable but eventual, so consumers
requiring portable evidence must wait until `attestation.available` is true
and must pin the public key outside the signer API.
Restart reconstruction charges the exact pending attestation reserve. A store
that becomes logically full after that correction remains open so signing or
retention can converge, but tenant/global growth fails until capacity is
released.
The revision-1 upgrade also inspects any pre-fix persisted attestation files:
exact tenant-bound rows are preserved, while unbound or malformed rows are
removed from availability and requeued for signing from authoritative job
state. Under explicit signing-off policy they remain unavailable and are
waived until a later signing-enabled restart reseeds them.
Legacy `ROOKHOLD_API_KEYS` tenant IDs must now meet the same identity contract as
indexed credentials and OIDC mappings: 1–128 safe printable ASCII characters.
Validate legacy tenant names before restart so every accepted job can be
encoded into the tenant-bound portable attestation profile.

## From v0.4.x to v0.5.0

v0.5 is an operator-interface release. It retains the v0.4 HTTP/OpenAPI
contract, schema-v4 database, execution providers, signed-evidence format,
identity model, and deployment boundary. There is no database migration.

Follow the standard drain, backup, canary, and rollback procedure, then replace
all binaries and SDK artifacts with the matching v0.5 release. Rebuild the
Compose image so its embedded dashboard and OCI version label are current, and
repeat the production verifier before restoring traffic. Existing v0.4 API and
MCP clients remain compatible, although their package versions should be kept
aligned with the server for support and telemetry clarity.

The embedded dashboard has a new transcript-first execution desk and docked run
composer. Browser credentials remain memory-only; operators must reconnect
after a reload. Revalidate keyboard operation, narrow-screen reflow, live event
reconnect, cancellation, artifact downloads, and requested-versus-observed
evidence on the browsers used by operators. Because v0.5 does not migrate the
database, a v0.4 binary rollback remains possible after draining v0.5, provided
the complete v0.4 binary/image/configuration set was retained and no separately
introduced configuration change prevents it.

## From v0.5.x to v0.6.0

v0.6 renames the project from Coop to Rookhold. It does not migrate the HTTP
API, schema-v4 database, signed-evidence format, execution providers, identity
model, or containment boundary. Existing v0.5 databases and evidence remain
valid.

Use the standard drain and backup procedure, then:

1. install the v0.6 `rookhold`, `rookhold-verify`,
   `rookhold-sandbox-init`, and `rookhold-oci-init` binaries from the same
   release; the corresponding `coop*` executable aliases are included for a
   staged migration;
2. rename operator configuration from `COOP_*` to `ROOKHOLD_*`. A v0.6 process
   falls back to the old name when the new one is absent, but rejects different
   non-empty values under both names;
3. install `deploy/rookhold.service` and
   `deploy/rookhold.env.example` under Rookhold paths. For an existing systemd
   installation, copy the database/jobs/key material into the new paths or set
   explicit `ROOKHOLD_DB`, `ROOKHOLD_JOBS_ROOT`, and
   `ROOKHOLD_ATTESTATION_KEY_FILE` values that point to the retained paths;
4. for Compose, keep `.coop-runtime`, the `coop-data` volume, and the
   `coop_attestation_key` secret name. v0.6 deliberately retains those storage
   identities so an in-place deployment does not start with a blank database
   or new signing key;
5. migrate SDK imports and MCP configuration when convenient: `rookhold` and
   `rookhold-mcp` are primary, while `coop`, `coop_mcp`, `coop-mcp`, and the old
   `coop_*` MCP tool calls remain aliases;
6. rebuild the image, run the production verifier and live SDK/MCP adapter,
   confirm the Rookhold dashboard branding, then restore traffic.

Do not rewrite existing evidence. `/v1`, `application/vnd.coop...` media types,
`coop://jobs/...` subject names, the original predicate-v1 URI, event/receipt
hashes, and `coop_*` metrics are durable compatibility identities. The GitHub
repository rename preserves the former URL redirect, but verifiers compare the
signed predicate URI as data and therefore continue to use the original Coop
URI.

Because there is no database migration, a drained rollback to v0.5 remains
possible with the complete v0.5 binary/image/configuration set. New Rookhold
paths and variables are not understood by v0.5, so retain the old deployment
files until rollback is no longer needed.

## From v0.6.x to v0.7.0

v0.7 adds terminal clients and host registrations without changing the HTTP
API, schema-v4 database, evidence formats, execution providers, or production
boundary. Existing v0.6 servers and evidence remain compatible, but keep the
server and SDK package versions aligned for support and telemetry.

After the standard drain, backup, image rebuild, and canary procedure, install
the v0.7 Python wheel in the operator-owned environment. It adds
`rookhold-cli` beside the existing `rookhold-mcp`; no bearer key is migrated or
written by the installer. Set `ROOKHOLD_API_KEY` in the launching environment,
open `rookhold-cli`, verify the displayed tenant/backend/isolation posture, run
`/mcp`, and submit one trusted canary.

Claude Code and OpenCode users can merge the new templates under
`integrations/`. Both launch the same stdio adapter and do not alter Rookhold's
server-side policy. Review the host's other shell/code tools separately: MCP
registration does not make Rookhold mandatory or disable a bypass route.

There is no database migration, so a drained rollback to v0.6 remains possible
with its complete binaries, SDKs, and deployment configuration. Remove or
disable the new CLI/MCP host registration before removing its v0.7 wheel.
