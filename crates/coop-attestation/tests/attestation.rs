use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE};
use base64::Engine as _;
use coop_attestation::{
    build_statement as build_tenant_statement,
    build_statement_from_receipt_json as build_tenant_statement_from_receipt_json,
    create_attestation as create_tenant_attestation, dsse_v1_pae, encode_statement, key_id,
    verify_attestation, ArtifactDigest, AttestationError, DsseEnvelope, SigningKey, StatementV1,
    SubjectArtifact, VerificationPolicy, VerifyingKey, COOP_EXECUTION_PREDICATE_TYPE,
    DSSE_PAYLOAD_TYPE, IN_TOTO_STATEMENT_TYPE, MAX_ENVELOPE_BYTES, MAX_SIGNATURES,
    MAX_STATEMENT_BYTES, MAX_TRUSTED_KEYS,
};
use ed25519_dalek::Signer as _;
use serde_json::{json, Value};
use std::num::NonZeroUsize;

const EXECUTION_ID: &str = "0191f8cf-57ef-7c14-a736-9b4b933eb84a";
const TENANT: &str = "tenant-vector";
const SUBJECT_NAME: &str = "urn:coop:result:0191f8cf-57ef-7c14-a736-9b4b933eb84a";
const SUBJECT_MEDIA_TYPE: &str = "application/vnd.coop.execution-result.v1+json";
const RESULT_BYTES: &[u8] = b"{\"exit_code\":0,\"status\":\"succeeded\",\"stdout\":\"42\\n\"}\n";

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn decode_hex(value: &str) -> Vec<u8> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    assert!(
        remainder.is_empty(),
        "hex test vector must contain byte pairs"
    );
    pairs
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

fn receipt() -> Value {
    json!({
        "event_chain": {
            "complete": true,
            "events": 4,
            "head": "4b5adf2fe8da9c90f6e0f4fcdf8f6f43f49ffaf2bd84cc84343320fbc3971e7e",
            "version": 1
        },
        "job_id": EXECUTION_ID,
        "outcome": "succeeded",
        "receipt_sha256": "700257184d9c8483dd82ebe6e2e0280c90a8d7274ad3aab51c885701f7240345",
        "version": 1
    })
}

fn subject() -> SubjectArtifact {
    SubjectArtifact::from_bytes(SUBJECT_NAME, SUBJECT_MEDIA_TYPE, RESULT_BYTES).unwrap()
}

fn expected_artifact() -> ArtifactDigest {
    ArtifactDigest::from_bytes(RESULT_BYTES)
}

fn build_statement(
    execution_id: &str,
    subject: &SubjectArtifact,
    receipt: Value,
) -> Result<StatementV1, AttestationError> {
    build_tenant_statement(TENANT, execution_id, subject, receipt)
}

fn build_statement_from_receipt_json(
    execution_id: &str,
    subject: &SubjectArtifact,
    receipt_json: &[u8],
) -> Result<StatementV1, AttestationError> {
    build_tenant_statement_from_receipt_json(TENANT, execution_id, subject, receipt_json)
}

fn create_attestation(
    execution_id: &str,
    subject: &SubjectArtifact,
    receipt: Value,
    signers: &[&SigningKey],
) -> Result<DsseEnvelope, AttestationError> {
    create_tenant_attestation(TENANT, execution_id, subject, receipt, signers)
}

fn statement_value() -> Value {
    let statement = build_statement(EXECUTION_ID, &subject(), receipt()).unwrap();
    serde_json::from_slice(&encode_statement(&statement).unwrap()).unwrap()
}

fn signed_envelope_json(signers: &[&SigningKey]) -> Vec<u8> {
    create_attestation(EXECUTION_ID, &subject(), receipt(), signers)
        .unwrap()
        .to_json_bytes()
        .unwrap()
}

fn manual_envelope(
    payload_type: &str,
    payload: &[u8],
    signatures: &[(&SigningKey, Option<&str>)],
) -> Vec<u8> {
    let pae = dsse_v1_pae(payload_type, payload).unwrap();
    let signatures = signatures
        .iter()
        .map(|(signing_key, hint)| {
            json!({
                "keyid": hint
                    .map(str::to_owned)
                    .unwrap_or_else(|| key_id(&signing_key.verifying_key())),
                "sig": BASE64_STANDARD.encode(signing_key.sign(&pae).to_bytes())
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&json!({
        "payloadType": payload_type,
        "payload": BASE64_STANDARD.encode(payload),
        "signatures": signatures
    }))
    .unwrap()
}

fn sign_statement_value(value: &Value, signing_key: &SigningKey) -> Vec<u8> {
    let payload = serde_json::to_vec(value).unwrap();
    manual_envelope(DSSE_PAYLOAD_TYPE, &payload, &[(signing_key, None)])
}

#[test]
fn official_dsse_pae_and_rfc8032_ed25519_vectors_match() {
    assert_eq!(
        dsse_v1_pae("http://example.com/HelloWorld", b"hello world").unwrap(),
        b"DSSEv1 29 http://example.com/HelloWorld 11 hello world"
    );

    let secret: [u8; 32] =
        decode_hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .try_into()
            .unwrap();
    let expected_public =
        decode_hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let expected_signature = decode_hex(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    );
    let key = SigningKey::from_bytes(&secret);
    assert_eq!(key.verifying_key().as_bytes(), expected_public.as_slice());
    assert_eq!(key.sign(b"").to_bytes(), expected_signature.as_slice());
}

#[test]
fn round_trip_authenticates_exact_statement_and_subject() {
    let key = signing_key(1);
    let envelope = signed_envelope_json(&[&key]);
    let verified = verify_attestation(
        &envelope,
        &expected_artifact(),
        &[key.verifying_key()],
        &VerificationPolicy::default()
            .with_subject_name(SUBJECT_NAME)
            .with_media_type(SUBJECT_MEDIA_TYPE),
    )
    .unwrap();

    assert_eq!(
        verified.statement().statement_type(),
        IN_TOTO_STATEMENT_TYPE
    );
    assert_eq!(
        verified.statement().predicate_type(),
        COOP_EXECUTION_PREDICATE_TYPE
    );
    assert_eq!(
        verified.statement().predicate().execution_id(),
        EXECUTION_ID
    );
    assert_eq!(verified.statement().predicate().tenant(), TENANT);
    assert_eq!(verified.subject().name(), SUBJECT_NAME);
    assert_eq!(verified.subject_sha256(), expected_artifact().sha256());
    assert_eq!(verified.verified_key_ids(), &[key_id(&key.verifying_key())]);

    let envelope_value: Value = serde_json::from_slice(&envelope).unwrap();
    let decoded = BASE64_STANDARD
        .decode(envelope_value["payload"].as_str().unwrap())
        .unwrap();
    assert_eq!(verified.statement_bytes(), decoded);

    let typed_envelope = create_attestation(EXECUTION_ID, &subject(), receipt(), &[&key]).unwrap();
    let typed_debug = format!("{typed_envelope:?}");
    let verified_debug = format!("{verified:?}");
    let receipt_hash = receipt()["receipt_sha256"].as_str().unwrap().to_string();
    for redacted in [typed_debug, verified_debug] {
        assert!(!redacted.contains(&receipt_hash));
        assert!(!redacted.contains("receipt_sha256"));
        assert!(redacted.contains("[REDACTED]"));
    }
}

#[test]
fn key_ids_are_hints_only() {
    let trusted = signing_key(2);
    let payload =
        encode_statement(&build_statement(EXECUTION_ID, &subject(), receipt()).unwrap()).unwrap();
    let wrong_hint = manual_envelope(
        DSSE_PAYLOAD_TYPE,
        &payload,
        &[(&trusted, Some("sha256:not-the-right-key"))],
    );
    assert!(verify_attestation(
        &wrong_hint,
        &expected_artifact(),
        &[trusted.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());

    let mut missing_hint: Value = serde_json::from_slice(&wrong_hint).unwrap();
    missing_hint["signatures"][0]
        .as_object_mut()
        .unwrap()
        .remove("keyid");
    assert!(verify_attestation(
        &serde_json::to_vec(&missing_hint).unwrap(),
        &expected_artifact(),
        &[trusted.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());

    let unusable_hint = manual_envelope(
        DSSE_PAYLOAD_TYPE,
        &payload,
        &[(&trusted, Some(&format!("{}\n", "x".repeat(300))))],
    );
    assert!(verify_attestation(
        &unusable_hint,
        &expected_artifact(),
        &[trusted.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());

    let attacker = signing_key(3);
    let forged_hint = manual_envelope(
        DSSE_PAYLOAD_TYPE,
        &payload,
        &[(&attacker, Some(&key_id(&trusted.verifying_key())))],
    );
    assert!(matches!(
        verify_attestation(
            &forged_hint,
            &expected_artifact(),
            &[trusted.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SignatureThresholdNotMet { .. })
    ));
}

#[test]
fn threshold_counts_distinct_trusted_keys_not_signature_entries() {
    let first = signing_key(4);
    let second = signing_key(5);
    let threshold_two =
        VerificationPolicy::default().with_minimum_signatures(NonZeroUsize::new(2).unwrap());

    let two_signers = signed_envelope_json(&[&second, &first]);
    let verified = verify_attestation(
        &two_signers,
        &expected_artifact(),
        &[second.verifying_key(), first.verifying_key()],
        &threshold_two,
    )
    .unwrap();
    assert_eq!(verified.verified_key_ids().len(), 2);
    assert!(verified
        .verified_key_ids()
        .windows(2)
        .all(|ids| ids[0] < ids[1]));

    assert!(matches!(
        verify_attestation(
            &two_signers,
            &expected_artifact(),
            &[first.verifying_key(), first.verifying_key()],
            &threshold_two,
        ),
        Err(AttestationError::InvalidSignatureThreshold {
            required: 2,
            available: 1
        })
    ));

    let mut duplicated: Value = serde_json::from_slice(&signed_envelope_json(&[&first])).unwrap();
    let duplicate_signature = duplicated["signatures"][0].clone();
    duplicated["signatures"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_signature);
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&duplicated).unwrap(),
            &expected_artifact(),
            &[first.verifying_key(), second.verifying_key()],
            &threshold_two,
        ),
        Err(AttestationError::SignatureThresholdNotMet {
            required: 2,
            verified: 1
        })
    ));

    assert!(matches!(
        create_attestation(EXECUTION_ID, &subject(), receipt(), &[&first, &first]),
        Err(AttestationError::DuplicateSigningKey)
    ));
}

#[test]
fn verify_before_parse_is_observable_on_malformed_payloads() {
    let trusted = signing_key(6);
    let malformed = b"{this is not JSON";
    let signed = manual_envelope(DSSE_PAYLOAD_TYPE, malformed, &[(&trusted, None)]);
    assert!(matches!(
        verify_attestation(
            &signed,
            &expected_artifact(),
            &[trusted.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidJson {
            document: "in-toto statement",
            ..
        })
    ));

    let mut invalid_signature: Value = serde_json::from_slice(&signed).unwrap();
    invalid_signature["signatures"][0]["sig"] = Value::String(BASE64_STANDARD.encode([0_u8; 64]));
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&invalid_signature).unwrap(),
            &expected_artifact(),
            &[trusted.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SignatureThresholdNotMet { .. })
    ));
}

#[test]
fn exact_bytes_are_signed_without_a_canonicalization_dependency() {
    let key = signing_key(7);
    let compact =
        encode_statement(&build_statement(EXECUTION_ID, &subject(), receipt()).unwrap()).unwrap();
    let parsed: Value = serde_json::from_slice(&compact).unwrap();
    let pretty = serde_json::to_vec_pretty(&parsed).unwrap();
    assert_ne!(compact, pretty);

    let pretty_signed = manual_envelope(DSSE_PAYLOAD_TYPE, &pretty, &[(&key, None)]);
    let verified = verify_attestation(
        &pretty_signed,
        &expected_artifact(),
        &[key.verifying_key()],
        &VerificationPolicy::default(),
    )
    .unwrap();
    assert_eq!(verified.statement_bytes(), pretty);

    let compact_signed = manual_envelope(DSSE_PAYLOAD_TYPE, &compact, &[(&key, None)]);
    let mut reformatted_without_resigning: Value = serde_json::from_slice(&compact_signed).unwrap();
    reformatted_without_resigning["payload"] = Value::String(BASE64_STANDARD.encode(pretty));
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&reformatted_without_resigning).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SignatureThresholdNotMet { .. })
    ));
}

#[test]
fn producer_normalizes_unsorted_receipt_objects_before_encoding() {
    let unsorted: Value = serde_json::from_str(
        r#"{"version":1,"receipt_sha256":"700257184d9c8483dd82ebe6e2e0280c90a8d7274ad3aab51c885701f7240345","outcome":"succeeded","job_id":"0191f8cf-57ef-7c14-a736-9b4b933eb84a","event_chain":{"version":1,"head":"4b5adf2fe8da9c90f6e0f4fcdf8f6f43f49ffaf2bd84cc84343320fbc3971e7e","events":4,"complete":true}}"#,
    )
    .unwrap();
    let normalized = build_statement(EXECUTION_ID, &subject(), receipt()).unwrap();
    let from_unsorted = build_statement(EXECUTION_ID, &subject(), unsorted).unwrap();
    assert_eq!(
        encode_statement(&from_unsorted).unwrap(),
        encode_statement(&normalized).unwrap()
    );
}

#[test]
fn payload_type_is_part_of_pae_and_checked_after_authentication() {
    let key = signing_key(8);
    let payload =
        encode_statement(&build_statement(EXECUTION_ID, &subject(), receipt()).unwrap()).unwrap();
    let wrong_type = manual_envelope("application/json", &payload, &[(&key, None)]);
    assert!(matches!(
        verify_attestation(
            &wrong_type,
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::PayloadTypeMismatch)
    ));

    let mut tampered: Value = serde_json::from_slice(&signed_envelope_json(&[&key])).unwrap();
    tampered["payloadType"] = Value::String("application/json".to_string());
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&tampered).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SignatureThresholdNotMet { .. })
    ));
}

#[test]
fn envelope_rejects_duplicates_and_bounds_signatures_but_ignores_extensions() {
    let key = signing_key(9);
    let envelope = signed_envelope_json(&[&key]);
    let value: Value = serde_json::from_slice(&envelope).unwrap();
    let duplicate = format!(
        "{{\"payloadType\":{},\"payload\":{},\"payload\":{},\"signatures\":{}}}",
        serde_json::to_string(value["payloadType"].as_str().unwrap()).unwrap(),
        serde_json::to_string(value["payload"].as_str().unwrap()).unwrap(),
        serde_json::to_string(value["payload"].as_str().unwrap()).unwrap(),
        serde_json::to_string(&value["signatures"]).unwrap(),
    );
    assert!(matches!(
        verify_attestation(
            duplicate.as_bytes(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidJson {
            document: "DSSE envelope",
            ..
        })
    ));

    let mut unknown = value.clone();
    unknown["trusted"] = Value::Bool(true);
    unknown["signatures"][0]["extension"] = json!({"future": true});
    assert!(verify_attestation(
        &serde_json::to_vec(&unknown).unwrap(),
        &expected_artifact(),
        &[key.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());

    let mut empty = value.clone();
    empty["signatures"] = json!([]);
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&empty).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::NoSignatures)
    ));

    let mut excessive = value;
    let entry = excessive["signatures"][0].clone();
    excessive["signatures"] = Value::Array(vec![entry; MAX_SIGNATURES + 1]);
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&excessive).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidJson {
            document: "DSSE envelope",
            ..
        })
    ));
}

#[test]
fn both_dsse_base64_alphabets_are_accepted_and_lengths_are_enforced() {
    let key = signing_key(10);
    let mut envelope: Value = serde_json::from_slice(&signed_envelope_json(&[&key])).unwrap();
    let payload = BASE64_STANDARD
        .decode(envelope["payload"].as_str().unwrap())
        .unwrap();
    let signature = BASE64_STANDARD
        .decode(envelope["signatures"][0]["sig"].as_str().unwrap())
        .unwrap();
    envelope["payload"] = Value::String(URL_SAFE.encode(payload));
    envelope["signatures"][0]["sig"] = Value::String(URL_SAFE.encode(signature));
    assert!(verify_attestation(
        &serde_json::to_vec(&envelope).unwrap(),
        &expected_artifact(),
        &[key.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());

    envelope["signatures"][0]["sig"] = Value::String("not+valid_url/_or_standard".into());
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&envelope).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidBase64 { field: "signature" })
    ));

    envelope["signatures"][0]["sig"] = Value::String(BASE64_STANDARD.encode([0_u8; 63]));
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&envelope).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidSignatureLength)
    ));

    let payload =
        encode_statement(&build_statement(EXECUTION_ID, &subject(), receipt()).unwrap()).unwrap();
    let mut malformed_trailing: Value = serde_json::from_slice(&manual_envelope(
        DSSE_PAYLOAD_TYPE,
        &payload,
        &[(&key, None), (&key, None)],
    ))
    .unwrap();
    malformed_trailing["signatures"][1]["sig"] = Value::String("not-base64".into());
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&malformed_trailing).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidBase64 { field: "signature" })
    ));

    let mut noncanonical_scalar: Value =
        serde_json::from_slice(&signed_envelope_json(&[&key])).unwrap();
    let mut signature = BASE64_STANDARD
        .decode(
            noncanonical_scalar["signatures"][0]["sig"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    signature[32..].fill(0xff);
    noncanonical_scalar["signatures"][0]["sig"] = Value::String(BASE64_STANDARD.encode(signature));
    assert!(matches!(
        verify_attestation(
            &serde_json::to_vec(&noncanonical_scalar).unwrap(),
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SignatureThresholdNotMet { .. })
    ));
}

#[test]
fn authenticated_statement_schema_and_cross_fields_are_enforced() {
    let key = signing_key(11);

    let mut wrong_statement_type = statement_value();
    wrong_statement_type["_type"] = Value::String("https://in-toto.io/Statement/v0.1".into());
    assert_schema_error(
        sign_statement_value(&wrong_statement_type, &key),
        &key.verifying_key(),
        |error| matches!(error, AttestationError::StatementTypeMismatch),
    );

    let mut wrong_predicate_type = statement_value();
    wrong_predicate_type["predicateType"] = Value::String("https://example.test/v1".into());
    assert_schema_error(
        sign_statement_value(&wrong_predicate_type, &key),
        &key.verifying_key(),
        |error| matches!(error, AttestationError::PredicateTypeMismatch),
    );

    let mut wrong_schema = statement_value();
    wrong_schema["predicate"]["schemaVersion"] = Value::from(2);
    assert_schema_error(
        sign_statement_value(&wrong_schema, &key),
        &key.verifying_key(),
        |error| matches!(error, AttestationError::PredicateSchemaVersionMismatch),
    );

    let mut decimal_schema = statement_value();
    decimal_schema["predicate"]["schemaVersion"] = json!(1.0);
    assert_schema_error(
        sign_statement_value(&decimal_schema, &key),
        &key.verifying_key(),
        |error| matches!(error, AttestationError::InvalidJson { .. }),
    );

    let mut missing_tenant = statement_value();
    missing_tenant["predicate"]
        .as_object_mut()
        .unwrap()
        .remove("tenant");
    assert_schema_error(
        sign_statement_value(&missing_tenant, &key),
        &key.verifying_key(),
        |error| matches!(error, AttestationError::InvalidJson { .. }),
    );

    let mut invalid_tenant = statement_value();
    invalid_tenant["predicate"]["tenant"] = Value::String("tenant\nother".into());
    assert_schema_error(
        sign_statement_value(&invalid_tenant, &key),
        &key.verifying_key(),
        |error| {
            matches!(
                error,
                AttestationError::InvalidProfileField {
                    field: "predicate.tenant"
                }
            )
        },
    );

    let mut cross_field = statement_value();
    cross_field["predicate"]["result"]["sha256"] = Value::String("a".repeat(64));
    assert_schema_error(
        sign_statement_value(&cross_field, &key),
        &key.verifying_key(),
        |error| matches!(error, AttestationError::SubjectDescriptorMismatch),
    );

    let mut no_subjects = statement_value();
    no_subjects["subject"] = json!([]);
    assert_schema_error(
        sign_statement_value(&no_subjects, &key),
        &key.verifying_key(),
        |error| {
            matches!(
                error,
                AttestationError::InvalidProfileField { field: "subject" }
            )
        },
    );

    let mut two_subjects = statement_value();
    let duplicated = two_subjects["subject"][0].clone();
    two_subjects["subject"]
        .as_array_mut()
        .unwrap()
        .push(duplicated);
    assert_schema_error(
        sign_statement_value(&two_subjects, &key),
        &key.verifying_key(),
        |error| {
            matches!(
                error,
                AttestationError::InvalidProfileField { field: "subject" }
            )
        },
    );

    let mut missing_sha256 = statement_value();
    missing_sha256["subject"][0]["digest"] = json!({"sha512":"ab"});
    assert_schema_error(
        sign_statement_value(&missing_sha256, &key),
        &key.verifying_key(),
        |error| {
            matches!(
                error,
                AttestationError::InvalidProfileField {
                    field: "subject.digest"
                }
            )
        },
    );

    let mut uppercase_sha256 = statement_value();
    uppercase_sha256["subject"][0]["digest"]["sha256"] = Value::String("A".repeat(64));
    assert_schema_error(
        sign_statement_value(&uppercase_sha256, &key),
        &key.verifying_key(),
        |error| {
            matches!(
                error,
                AttestationError::InvalidProfileField {
                    field: "subject.digest.sha256"
                }
            )
        },
    );

    let mut extra_digest = statement_value();
    extra_digest["subject"][0]["digest"]["sha512"] = Value::String("b".repeat(128));
    assert!(verify_attestation(
        &sign_statement_value(&extra_digest, &key),
        &expected_artifact(),
        &[key.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());

    let mut extensions = statement_value();
    extensions["futureStatementField"] = Value::Bool(true);
    extensions["subject"][0]["futureResourceField"] = Value::from(1);
    extensions["predicate"]["futurePredicateField"] = Value::Bool(true);
    extensions["predicate"]["result"]["futureResultField"] = Value::from("ignored");
    assert!(verify_attestation(
        &sign_statement_value(&extensions, &key),
        &expected_artifact(),
        &[key.verifying_key()],
        &VerificationPolicy::default()
    )
    .is_ok());
}

#[test]
fn profile_constructor_rejects_ambiguous_identifiers_and_media_types() {
    for media_type in [
        "",
        "application",
        "application//json",
        "application/json text",
    ] {
        assert!(SubjectArtifact::from_bytes("urn:result", media_type, b"{}").is_err());
    }
    assert!(SubjectArtifact::from_bytes("contains\ncontrol", "application/json", b"{}").is_err());
    assert!(build_statement("", &subject(), receipt()).is_err());
    assert!(build_tenant_statement("", EXECUTION_ID, &subject(), receipt()).is_err());
}

#[test]
fn predicate_binds_the_current_coop_receipt_core() {
    // Existing v0.3 receipts do not contain tenant. The predicate obtains it
    // independently from the authoritative job row and remains backfillable.
    assert_eq!(
        build_statement(EXECUTION_ID, &subject(), receipt())
            .unwrap()
            .predicate()
            .tenant(),
        TENANT
    );

    let mut wrong_tenant = receipt();
    wrong_tenant["tenant"] = Value::String("tenant-other".into());
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), wrong_tenant),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.tenant"
        })
    ));

    let mut wrong_job = receipt();
    wrong_job["job_id"] = Value::String("different-job".into());
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), wrong_job),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.job_id"
        })
    ));

    let mut bad_outcome = receipt();
    bad_outcome["outcome"] = Value::String("running".into());
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), bad_outcome),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.outcome"
        })
    ));

    let mut bad_digest = receipt();
    bad_digest["receipt_sha256"] = Value::String("A".repeat(64));
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), bad_digest),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.receipt_sha256"
        })
    ));

    let mut wrong_checksum = receipt();
    wrong_checksum["receipt_sha256"] = Value::String("0".repeat(64));
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), wrong_checksum),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.receipt_sha256"
        })
    ));

    let mut missing_chain = receipt();
    missing_chain.as_object_mut().unwrap().remove("event_chain");
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), missing_chain),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.event_chain"
        })
    ));

    let mut deeply_nested = Value::Null;
    for _ in 0..65 {
        deeply_nested = json!({"nested": deeply_nested});
    }
    let mut deep_receipt = receipt();
    deep_receipt["extension"] = deeply_nested;
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), deep_receipt),
        Err(AttestationError::InvalidProfileField {
            field: "predicate.receipt.depth"
        })
    ));

    let mut oversized_receipt = receipt();
    oversized_receipt["extension"] = Value::String("x".repeat(200 * 1024));
    assert!(matches!(
        build_statement(EXECUTION_ID, &subject(), oversized_receipt),
        Err(AttestationError::ReceiptTooComplex { .. })
    ));
}

#[test]
fn verifier_and_pae_enforce_fixed_allocation_limits() {
    let key = signing_key(14);
    assert!(matches!(
        dsse_v1_pae(DSSE_PAYLOAD_TYPE, &vec![0_u8; MAX_STATEMENT_BYTES + 1]),
        Err(AttestationError::StatementTooLarge { .. })
    ));
    assert!(matches!(
        verify_attestation(
            &vec![b' '; MAX_ENVELOPE_BYTES + 1],
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::EnvelopeTooLarge { .. })
    ));

    let envelope = signed_envelope_json(&[&key]);
    let trusted = (0..=MAX_TRUSTED_KEYS)
        .map(|index| signing_key(index as u8 + 30).verifying_key())
        .collect::<Vec<_>>();
    assert!(matches!(
        verify_attestation(
            &envelope,
            &expected_artifact(),
            &trusted,
            &VerificationPolicy::default()
        ),
        Err(AttestationError::TooManyTrustedKeys { .. })
    ));
}

#[test]
fn duplicate_keys_inside_authenticated_receipt_are_rejected() {
    let key = signing_key(12);
    let valid = serde_json::to_string(&statement_value()).unwrap();
    let payload = valid.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    let envelope = manual_envelope(DSSE_PAYLOAD_TYPE, payload.as_bytes(), &[(&key, None)]);
    assert!(matches!(
        verify_attestation(
            &envelope,
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::InvalidJson {
            document: "in-toto statement",
            ..
        })
    ));
}

#[test]
fn server_receipt_json_entry_point_rejects_duplicates_before_signing() {
    let valid = serde_json::to_vec(&receipt()).unwrap();
    let statement = build_statement_from_receipt_json(EXECUTION_ID, &subject(), &valid).unwrap();
    assert_eq!(statement.predicate().receipt(), &receipt());

    assert!(matches!(
        build_statement_from_receipt_json(
            EXECUTION_ID,
            &subject(),
            br#"{"job_id":"a","job_id":"b"}"#,
        ),
        Err(AttestationError::InvalidJson {
            document: "Rookhold receipt",
            ..
        })
    ));
}

#[test]
fn artifact_digest_size_and_policy_are_independently_checked() {
    let key = signing_key(13);
    let envelope = signed_envelope_json(&[&key]);
    assert!(matches!(
        verify_attestation(
            &envelope,
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default().with_tenant("tenant-other")
        ),
        Err(AttestationError::TenantPolicyMismatch)
    ));
    let wrong_bytes = ArtifactDigest::from_bytes(b"different result");
    assert!(matches!(
        verify_attestation(
            &envelope,
            &wrong_bytes,
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SubjectDigestMismatch)
    ));

    let wrong_size = ArtifactDigest::from_precomputed(expected_artifact().sha256(), 999).unwrap();
    assert!(matches!(
        verify_attestation(
            &envelope,
            &wrong_size,
            &[key.verifying_key()],
            &VerificationPolicy::default()
        ),
        Err(AttestationError::SubjectSizeMismatch)
    ));

    assert!(matches!(
        verify_attestation(
            &envelope,
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default().with_subject_name("urn:wrong")
        ),
        Err(AttestationError::SubjectPolicyMismatch { field: "name" })
    ));
    assert!(matches!(
        verify_attestation(
            &envelope,
            &expected_artifact(),
            &[key.verifying_key()],
            &VerificationPolicy::default().with_media_type("text/plain")
        ),
        Err(AttestationError::SubjectPolicyMismatch {
            field: "media type"
        })
    ));
}

fn assert_schema_error(
    envelope: Vec<u8>,
    trusted: &VerifyingKey,
    predicate: impl FnOnce(&AttestationError) -> bool,
) {
    let error = verify_attestation(
        &envelope,
        &expected_artifact(),
        &[*trusted],
        &VerificationPolicy::default(),
    )
    .unwrap_err();
    assert!(predicate(&error), "unexpected error: {error:?}");
}
