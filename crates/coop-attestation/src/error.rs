use std::io;

/// Errors returned by the attestation encoder, signer, verifier, and key I/O helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttestationError {
    /// The serialized DSSE envelope exceeded the component's parsing limit.
    #[error("DSSE envelope exceeds the {max_bytes}-byte limit")]
    EnvelopeTooLarge { max_bytes: usize },

    /// The decoded in-toto statement exceeded the component's parsing limit.
    #[error("in-toto statement exceeds the {max_bytes}-byte limit")]
    StatementTooLarge { max_bytes: usize },

    /// The result artifact exceeded the offline verifier's bounded read limit.
    #[error("result artifact exceeds the {max_bytes}-byte limit")]
    SubjectArtifactTooLarge { max_bytes: usize },

    /// An offline-verifier input was not a non-symlink regular file.
    #[error("verifier input must be a non-symlink regular file")]
    UnsafeInputFileType,

    /// An in-memory receipt exceeded the pre-normalization complexity budget.
    #[error("embedded receipt exceeds the {max_nodes}-node or {max_bytes}-byte complexity budget")]
    ReceiptTooComplex { max_nodes: usize, max_bytes: usize },

    /// A JSON document was malformed, had duplicate keys, or did not match its strict profile.
    #[error("{document} JSON is invalid at line {line}, column {column}")]
    InvalidJson {
        document: &'static str,
        line: usize,
        column: usize,
    },

    /// A JSON document could not be serialized.
    #[error("could not encode {document} JSON")]
    JsonEncoding { document: &'static str },

    /// The DSSE envelope did not contain a signature.
    #[error("DSSE envelope must contain at least one signature")]
    NoSignatures,

    /// A signing request exceeded the profile's signature count.
    #[error("DSSE signing request contains more than {max} signatures")]
    TooManySignatures { max: usize },

    /// A DSSE base64 field was neither valid standard nor URL-safe base64.
    #[error("DSSE {field} is not valid standard or URL-safe base64")]
    InvalidBase64 { field: &'static str },

    /// A decoded Ed25519 signature had the wrong byte length.
    #[error("DSSE signature is not a 64-byte Ed25519 signature")]
    InvalidSignatureLength,

    /// No signing key was provided.
    #[error("at least one Ed25519 signing key is required")]
    NoSigningKeys,

    /// The same signing key was supplied more than once.
    #[error("duplicate Ed25519 signing key")]
    DuplicateSigningKey,

    /// Two distinct public keys produced the same derived key identifier.
    #[error("derived Ed25519 key ID collision")]
    KeyIdCollision,

    /// No trusted verification key was provided.
    #[error("at least one trusted Ed25519 public key is required")]
    NoTrustedKeys,

    /// The configured trust set exceeded the verifier's fixed CPU bound.
    #[error("trusted Ed25519 key set exceeds the {max} key limit")]
    TooManyTrustedKeys { max: usize },

    /// The configured signature threshold was invalid for the trust set.
    #[error("signature threshold {required} exceeds {available} unique trusted keys")]
    InvalidSignatureThreshold { required: usize, available: usize },

    /// Too few distinct trusted keys produced valid Ed25519 signatures.
    #[error("DSSE signature threshold was not met (required {required}, verified {verified})")]
    SignatureThresholdNotMet { required: usize, verified: usize },

    /// The authenticated DSSE payload type was not the Rookhold profile's in-toto media type.
    #[error("authenticated DSSE payloadType is not application/vnd.in-toto+json")]
    PayloadTypeMismatch,

    /// The in-toto statement type was not Statement/v1.
    #[error("authenticated payload is not an in-toto Statement/v1")]
    StatementTypeMismatch,

    /// The statement predicate type was not the versioned Rookhold execution predicate.
    #[error("authenticated statement has an unsupported predicateType")]
    PredicateTypeMismatch,

    /// The Rookhold predicate schema version was unsupported.
    #[error("authenticated Rookhold execution predicate has an unsupported schemaVersion")]
    PredicateSchemaVersionMismatch,

    /// A required profile field was empty, too long, or otherwise malformed.
    #[error("authenticated Rookhold execution statement has an invalid {field} field")]
    InvalidProfileField { field: &'static str },

    /// The statement subject and predicate result descriptors did not agree.
    #[error("authenticated subject and predicate result descriptors disagree")]
    SubjectDescriptorMismatch,

    /// The supplied result artifact did not match the authenticated SHA-256 digest.
    #[error("result artifact SHA-256 does not match the authenticated subject")]
    SubjectDigestMismatch,

    /// The supplied result artifact did not match the authenticated byte length.
    #[error("result artifact size does not match the authenticated predicate")]
    SubjectSizeMismatch,

    /// A caller-provided subject expectation did not match the authenticated value.
    #[error("authenticated result artifact {field} does not match the verifier policy")]
    SubjectPolicyMismatch { field: &'static str },

    /// The authenticated execution tenant did not match caller policy.
    #[error("authenticated execution tenant does not match the verifier policy")]
    TenantPolicyMismatch,

    /// OS randomness was unavailable during key generation.
    #[error("operating-system randomness is unavailable")]
    RandomnessUnavailable,

    /// A private or public key did not use the component's canonical PEM profile.
    #[error("{kind} key file is not canonical {format}")]
    InvalidKeyEncoding {
        kind: &'static str,
        format: &'static str,
    },

    /// A key file was not a regular file or was reached through a symlink.
    #[error("{kind} key path must be a non-symlink regular file")]
    UnsafeKeyFileType { kind: &'static str },

    /// A key file exceeded its small fixed parsing limit.
    #[error("{kind} key file exceeds the {max_bytes}-byte limit")]
    KeyFileTooLarge {
        kind: &'static str,
        max_bytes: usize,
    },

    /// Unix key-file mode bits allowed unsafe mutation or disclosure.
    #[error("{kind} key file has unsafe Unix permission bits")]
    UnsafeKeyFilePermissions { kind: &'static str },

    /// A Unix key file was owned by an identity outside the accepted trust boundary.
    #[error("{kind} key file has an unsafe Unix owner")]
    UnsafeKeyFileOwner { kind: &'static str },

    /// A target key file already existed; key operations never overwrite files.
    #[error("refusing to overwrite an existing key file")]
    KeyFileAlreadyExists,

    /// A filesystem or streaming-artifact operation failed.
    #[error("I/O operation failed: {0}")]
    Io(#[from] io::Error),
}

pub(crate) fn invalid_json(document: &'static str, error: serde_json::Error) -> AttestationError {
    AttestationError::InvalidJson {
        document,
        line: error.line(),
        column: error.column(),
    }
}
