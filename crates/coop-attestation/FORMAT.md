# Rookhold execution attestation profile

This component implements a narrow portable format for signing one Rookhold
execution result. It composes three independent contracts:

1. in-toto Attestation Framework `Statement/v1`;
2. the Rookhold execution predicate v1 defined below;
3. DSSE v1 pre-authentication encoding signed with Ed25519.

The profile authenticates what a Rookhold control plane claims it observed. A
valid signature does not prove that the claim is true, that an external side
effect happened, that execution was deterministic, or that the executor was a
TEE or remotely attested environment.

## Envelope

The outer object is a JSON DSSE envelope:

```json
{
  "payloadType": "application/vnd.in-toto+json",
  "payload": "BASE64_EXACT_STATEMENT_BYTES",
  "signatures": [{"keyid": "sha256:...", "sig": "BASE64_64_BYTE_SIGNATURE"}]
}
```

The signature input is exactly:

```text
PAE(type, body) = "DSSEv1" SP LEN(type) SP type SP LEN(body) SP body
```

`LEN` is the ASCII decimal byte length with no leading zero. The producer
emits padded standard Base64. In accordance with DSSE v1.0.2 and RFC 4648,
verifiers accept padded standard or padded URL-safe Base64.

`keyid` is an unauthenticated hint. Rookhold emits
`sha256:<lowercase SHA-256 of the raw 32-byte Ed25519 public key>`. The verifier
may use the hint to choose trial order, but a signature is accepted only when
one of the configured trusted public keys verifies it. Thresholds count
distinct trusted public keys, never signature entries.

Unknown envelope and signature fields are ignored for DSSE forward
compatibility. Duplicate JSON keys are rejected at every depth because their
interpretation differs among JSON implementations.

## Statement

The decoded payload is a JSON in-toto Statement:

```json
{
  "_type": "https://in-toto.io/Statement/v1",
  "subject": [{
    "name": "urn:coop:result:JOB_ID",
    "digest": {"sha256": "LOWERCASE_HEX"},
    "mediaType": "application/vnd.coop.execution-result.v1+json"
  }],
  "predicateType": "https://github.com/sambai-dev/coop/blob/main/crates/coop-attestation/FORMAT.md#predicate-v1",
  "predicate": {"...": "..."}
}
```

This profile has exactly one subject and requires its SHA-256 digest. Other
digest algorithms and unrecognized in-toto fields may be present and are
ignored. The verifier hashes the separately supplied result artifact and
compares its exact byte length and SHA-256 with the authenticated predicate.
It also requires the subject and predicate result name, media type, and
SHA-256 to agree. Name and media type are policy metadata; in-toto itself
matches subjects by digest.

The verifier authenticates the DSSE PAE before parsing these statement bytes.
It returns the same verified bytes to the application layer. It does not
re-parse the envelope, normalize JSON, or substitute a reserialized payload.
The producer's compact JSON encoding is deterministic for the typed profile,
but it is not an RFC 8785 claim and canonical JSON is not required for
verification.

Unknown Statement, ResourceDescriptor, and predicate fields are ignored under
the in-toto monotonic parsing model. Known required fields and their types are
still validated. Duplicate keys remain invalid.

## Predicate v1

The predicate type URI above carries the major version. A breaking semantic or
wire change requires a new URI. The redundant integer `schemaVersion` must be
`1`; it is a local decoder guard and allows operators to diagnose a producer
that incorrectly reuses the v1 predicate URI for an incompatible shape. It
does not permit a different schema under the same predicate URI.

```json
{
  "schemaVersion": 1,
  "executionId": "JOB_ID",
  "tenant": "TENANT_ID",
  "result": {
    "name": "urn:coop:result:JOB_ID",
    "mediaType": "application/vnd.coop.execution-result.v1+json",
    "sizeBytes": 123,
    "sha256": "LOWERCASE_HEX"
  },
  "receipt": {
    "version": 1,
    "job_id": "JOB_ID",
    "outcome": "succeeded",
    "receipt_sha256": "LOWERCASE_HEX",
    "event_chain": {
      "version": 1,
      "head": "LOWERCASE_HEX",
      "events": 4,
      "complete": true
    }
  }
}
```

`executionId`, `tenant`, `result`, and `receipt` are required. `tenant` is the
1–128 character authoritative owner identity read from the durable job row,
not from a download request or receipt extension. `receipt` is the existing
Rookhold v1 terminal receipt. Its `job_id` must equal `executionId`; its outcome,
receipt SHA-256, and v1 event-chain core are structurally validated. A receipt
may omit tenant so v0.3 terminal rows remain byte-for-byte backfillable. If a
receipt extension named `tenant` is present, it must equal the predicate
tenant. The predicate uses lowerCamelCase following in-toto predicate
guidance. The embedded receipt retains Rookhold's existing snake_case field names
and may carry additional fields.

Predicate v1 was finalized with required tenant binding before the first
release of portable Rookhold attestations. No supported v0.3 release emitted this
profile, so this closes the contract without creating an intentionally
unbound legacy attestation variant.

The predicate URI deliberately retains the former `sambai-dev/coop` repository
path. It is signed wire identity, not a documentation redirect, and changing it
would make v1 evidence incompatible with existing verifiers.

### Rookhold receipt hash

The attestation verifies the existing Rookhold receipt v1 checksum before signing
and after DSSE verification. Its canonicalization is the same versioned Rookhold
algorithm used by `coop-store`:

1. remove the top-level `receipt_sha256` member;
2. encode UTF-8 JSON with no insignificant whitespace;
3. retain array order;
4. sort every object's keys lexicographically;
5. encode null, booleans, and numbers with `serde_json::Value` token spelling;
6. encode keys and strings with `serde_json` JSON escaping;
7. SHA-256 the resulting bytes and lowercase-hex encode the digest.

All receipt extension fields participate. Rookhold v1's own numeric evidence fields
are integers. An extension that depends on cross-language floating-point
canonicalization should use strings or define its own independently hashed
artifact; this v1 algorithm is not an RFC 8785 claim.

The machine-readable profile is
[`schema/coop-execution-statement-v1.schema.json`](schema/coop-execution-statement-v1.schema.json).
Normative behavior that JSON Schema cannot express—signature-first parsing,
threshold trust, actual artifact hashing, and subject/result equality—is
enforced by the library and test vectors. Runtime integer fields must also be
lexical JSON integers accepted by Rust `u32`/`u64` decoding; JSON Schema treats
some mathematically integral decimal spellings such as `1.0` as integers even
though this profile rejects them.

## Ed25519 key files

Private keys use one canonical representation:

- unencrypted Ed25519 PKCS#8 PEM;
- label `PRIVATE KEY`;
- LF line endings;
- one final LF;
- exact DER/PEM output produced by this crate.

Public keys use canonical Ed25519 SubjectPublicKeyInfo PEM with the same LF
rules and label `PUBLIC KEY`. The strict encoding check prevents ambiguous
files, concatenated objects, encrypted-key surprises, and ignored trailing
data.

Key writes use create-new semantics and never overwrite. On Unix, private keys
are created mode `0600`; reads reject symlinks, non-regular files, any
group/other permission, executable bit, or special mode bit. Public keys are
created `0644`; reads reject group/other-writable, executable, or special-mode
files. Private files must be owned by the effective service user. Public trust
roots must be owned by that user or root. Windows ACL validation
is outside the standard library's portable guarantees, so Windows operators
must protect private keys with an ACL appropriate to the service identity.
The Windows file helpers do not establish or validate that ACL and their
reparse-point check is best-effort rather than an atomic `O_NOFOLLOW`
equivalent. Treat them as development/convenience I/O unless the destination
directory already has a reviewed service-only ACL; provision production keys
through an OS secret facility or protected deployment mechanism.

On Unix, every original parent component is inspected before resolution.
Symlink aliases are accepted only when the symlink itself is root-owned, so
platform aliases such as macOS `/tmp -> /private/tmp` work while an
attacker-owned `/tmp/link` does not. The resolved parent chain is checked again,
and the final file is opened through it with `O_NOFOLLOW`. Every ordinary
ancestor must be owned by root or the effective service user and not
group/other-writable. A root-owned sticky directory such as `/tmp` is allowed.
Mutation of the original alias after resolution cannot redirect the final open.

The implementation never prints, logs, or embeds private key material.

## Limits and privacy

- DSSE envelope: 2 MiB maximum.
- decoded statement: 1 MiB maximum.
- signatures: 32 maximum.
- configured trusted public keys: 16 maximum.
- PEM key file: 16 KiB maximum.
- embedded receipt JSON depth: 64 maximum.
- authenticated tenant: 128 characters maximum.
- embedded receipt complexity: 65,536 nodes and a 768 KiB conservative
  pre-normalization byte estimate.
- offline CLI result-artifact file: 16 MiB maximum (the library supports
  incremental precomputation for larger artifacts).

The verifier therefore performs at most 512 Ed25519 key/signature attempts and
stops earlier when the configured distinct-key threshold is met. Signature
entries are count-bounded during deserialization, before a large `Vec` can be
allocated.

The result bytes are not embedded; only their digest, length, name, and media
type are. The receipt may still contain sensitive metadata. Do not publish an
attestation or place it in a public transparency log without a disclosure and
retention policy. Plain SHA-256 of low-entropy data can be dictionary-tested.
