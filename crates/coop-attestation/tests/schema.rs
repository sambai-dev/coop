use coop_attestation::{build_statement, encode_statement, SubjectArtifact};
use serde_json::{json, Value};

const SCHEMA: &str = include_str!("../schema/coop-execution-statement-v1.schema.json");

fn statement() -> Value {
    let subject = SubjectArtifact::from_bytes(
        "urn:coop:result:schema-test",
        "application/vnd.coop.execution-result.v1+json",
        b"{}\n",
    )
    .unwrap();
    let statement = build_statement(
        "tenant-schema",
        "schema-test",
        &subject,
        json!({
            "event_chain": {
                "complete": true,
                "events": 1,
                "head": "a".repeat(64),
                "version": 1
            },
            "job_id":"schema-test",
            "outcome":"succeeded",
            "receipt_sha256": "da4b699d6a20890ec35a0de42d35b4a399607d19bbb035956f6c7ee0074d7326",
            "version":1
        }),
    )
    .unwrap();
    serde_json::from_slice(&encode_statement(&statement).unwrap()).unwrap()
}

#[test]
fn emitted_statement_validates_against_published_schema() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&statement()));
}

#[test]
fn schema_rejects_wrong_known_fields_and_permits_monotonic_extensions() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let mut wrong_type = statement();
    wrong_type["predicate"]["schemaVersion"] = Value::from(2);
    assert!(!validator.is_valid(&wrong_type));

    let mut missing_tenant = statement();
    missing_tenant["predicate"]
        .as_object_mut()
        .unwrap()
        .remove("tenant");
    assert!(!validator.is_valid(&missing_tenant));

    let mut wrong_digest = statement();
    wrong_digest["subject"][0]["digest"]["sha256"] = Value::String("ABC".into());
    assert!(!validator.is_valid(&wrong_digest));

    // JSON Schema defines 1.0 as mathematically integral. FORMAT.md records
    // that the runtime profile deliberately requires lexical Rust integers.
    let mut decimal_integer = statement();
    decimal_integer["predicate"]["schemaVersion"] = json!(1.0);
    assert!(validator.is_valid(&decimal_integer));

    let mut extended = statement();
    extended["futureStatementField"] = Value::Bool(true);
    extended["subject"][0]["futureResourceField"] = Value::from(1);
    extended["predicate"]["futurePredicateField"] = Value::Bool(true);
    assert!(validator.is_valid(&extended));
}
