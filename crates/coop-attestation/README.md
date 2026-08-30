# coop-attestation

`coop-attestation` is the isolated signing and verification component for
portable Coop execution evidence. It does not modify the server, scheduler,
store, API, or existing receipt format.

The default `cli` feature builds `coop-verify`. A server that only needs the
library may depend on the crate with `default-features = false`.

It provides:

- a typed in-toto Statement/v1 Coop predicate;
- exact-byte DSSE v1 PAE;
- Ed25519 multi-signature and distinct-key threshold verification;
- result-artifact SHA-256 and schema/profile validation;
- strict canonical PKCS#8/SPKI key files;
- the `coop-verify` key-management and offline-verification CLI.

Read [FORMAT.md](FORMAT.md) before integrating it. In particular, `keyid` is
only a hint, statement parsing occurs only after signature verification, and a
signature authenticates an issuer's claim rather than proving execution truth.

## Library sketch

```rust
use coop_attestation::{create_attestation, SubjectArtifact};
use serde_json::json;

# fn example(signing_key: &coop_attestation::SigningKey) -> Result<(), coop_attestation::AttestationError> {
let result = br#"{"status":"succeeded"}"#;
let subject = SubjectArtifact::from_bytes(
    "urn:coop:result:job-id",
    "application/vnd.coop.execution-result.v1+json",
    result,
)?;
let envelope = create_attestation(
    "tenant-id",
    "job-id",
    &subject,
    json!({
        "version": 1,
        "job_id": "job-id",
        "outcome": "succeeded",
        "receipt_sha256": "3f086f459292130062c420b846dfb7e4140c17d8173ae2537a5c902122568651",
        "event_chain": {
            "version": 1,
            "head": "1".repeat(64),
            "events": 1,
            "complete": true
        }
    }),
    &[signing_key],
)?;
let wire_bytes = envelope.to_json_bytes()?;
# let _ = wire_bytes;
# Ok(())
# }
```

Verification accepts a precomputed [`ArtifactDigest`](src/format.rs#L44)
so a server can hash large result files incrementally. The offline CLI instead
requires a non-symlink regular file and caps it at 16 MiB to match the server's
exact result-artifact ceiling while avoiding FIFO/device
hangs and unbounded reads from attacker-controlled workspaces. The library
returns the exact verified statement bytes alongside the typed view.

## CLI

```text
coop-verify generate-key --output coop-attestation.pem
coop-verify public-key \
  --private-key coop-attestation.pem \
  --output coop-attestation.pub.pem
coop-verify verify \
  --envelope job.dsse.json \
  --subject job-result.json \
  --public-key coop-attestation.pub.pem \
  --tenant tenant-id
```

Repeat `--public-key` for rotation or threshold policies and set
`--threshold N` to require `N` distinct trusted signers. Verification output
contains the authenticated tenant, identifiers, digests, outcome, and
chain-completeness only; it does
not print the embedded receipt.
Exit zero means the attestation is authentic and profile-valid, not that the
job succeeded. Automation should supply `--tenant` from its own expected
identity context, compare `execution_id`, and inspect the returned `outcome`
and `event_chain_complete` fields against its own policy.

## Server integration

Coop schema v4 now implements this boundary with a durable signing outbox:

1. terminal receipt and outbox work commit together;
2. the trusted control plane binds `jobs.tenant` into both the predicate and
   deterministic exact result artifact;
3. it signs only after receipt finalization and self-verifies before storage;
4. persistence is conditional on the same receipt bytes and is idempotent;
5. restart backfills retained terminal rows without changing legacy v0.3
   receipt bytes or their digest;
6. tenant-scoped endpoints return exact artifact/envelope bytes and digests;
7. production requires a key unless `COOP_ATTESTATION_MODE=off` is explicit.

Signing is intentionally eventual. The current public-key endpoint is
discovery metadata, not trust bootstrap; operators must pin/distribute keys
out of band and retain prior keys through rotation. The integrated signer uses
a local PEM rather than a KMS/HSM, and the signature is not hardware remote
attestation.

When the receipt comes from SQLite as JSON text, use
`build_statement_from_receipt_json`; it performs the duplicate-key check before
constructing the typed predicate.
