use coop_attestation::{
    build_statement, decode_public_key_pem, encode_public_key_pem, encode_statement,
    sign_statement, verify_attestation, ArtifactDigest, SigningKey, SubjectArtifact,
    VerificationPolicy,
};
use serde_json::Value;

const PUBLIC_KEY: &str = include_str!("vectors/v1/public-key.pem");
const SUBJECT: &[u8] = include_bytes!("vectors/v1/subject.json");
const RECEIPT: &str = include_str!("vectors/v1/receipt.json");
const STATEMENT: &str = include_str!("vectors/v1/statement.json");
const ENVELOPE: &str = include_str!("vectors/v1/envelope.json");

#[test]
fn frozen_v1_vector_reproduces_and_verifies() {
    let seed = std::array::from_fn(|index| index as u8);
    let signing_key = SigningKey::from_bytes(&seed);
    let public_key = decode_public_key_pem(PUBLIC_KEY).unwrap();
    assert_eq!(public_key, signing_key.verifying_key());
    assert_eq!(
        encode_public_key_pem(&signing_key.verifying_key()).unwrap(),
        PUBLIC_KEY
    );

    let subject = SubjectArtifact::from_bytes(
        "urn:coop:result:0191f8cf-57ef-7c14-a736-9b4b933eb84a",
        "application/vnd.coop.execution-result.v1+json",
        SUBJECT,
    )
    .unwrap();
    let receipt: Value = serde_json::from_str(RECEIPT).unwrap();
    let statement = build_statement(
        "tenant-vector",
        "0191f8cf-57ef-7c14-a736-9b4b933eb84a",
        &subject,
        receipt,
    )
    .unwrap();
    let statement_bytes = encode_statement(&statement).unwrap();
    let envelope = sign_statement(&statement, &[&signing_key])
        .unwrap()
        .to_json_bytes()
        .unwrap();
    assert_eq!(statement_bytes, strip_fixture_newline(STATEMENT.as_bytes()));

    assert_eq!(envelope, strip_fixture_newline(ENVELOPE.as_bytes()));

    let verified = verify_attestation(
        &envelope,
        &ArtifactDigest::from_bytes(SUBJECT),
        &[public_key],
        &VerificationPolicy::default(),
    )
    .unwrap();
    assert_eq!(verified.statement_bytes(), statement_bytes);
    assert_eq!(verified.statement().predicate().tenant(), "tenant-vector");
}

fn strip_fixture_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
