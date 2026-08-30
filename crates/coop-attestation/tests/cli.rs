#![cfg(feature = "cli")]

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use coop_attestation::{
    create_attestation, dsse_v1_pae, read_private_key_file, ArtifactDigest, SubjectArtifact,
    DSSE_PAYLOAD_TYPE,
};
use ed25519_dalek::Signer as _;
use serde_json::{json, Value};
use std::fs;
use std::process::{Command, Output};

const RESULT: &[u8] = b"{\"status\":\"succeeded\"}\n";

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_coop-verify"))
}

fn run(command: &mut Command) -> Output {
    command.output().expect("run coop-verify")
}

#[test]
fn generate_public_and_verify_workflow_never_prints_private_material() {
    let temp = tempfile::tempdir().unwrap();
    let private_path = temp.path().join("attestation.pem");
    let public_path = temp.path().join("attestation.pub.pem");
    let envelope_path = temp.path().join("attestation.dsse.json");
    let legacy_envelope_path = temp.path().join("legacy-unbound.dsse.json");
    let subject_path = temp.path().join("result.json");

    let generated = run(cli().arg("generate-key").arg("--output").arg(&private_path));
    assert!(generated.status.success(), "{:?}", generated.stderr);
    assert!(!generated.stdout.windows(11).any(|w| w == b"PRIVATE KEY"));
    assert!(!generated.stderr.windows(11).any(|w| w == b"PRIVATE KEY"));
    let generated_json: Value = serde_json::from_slice(&generated.stdout).unwrap();
    assert!(generated_json["key_id"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let refused_overwrite = run(cli().arg("generate-key").arg("--output").arg(&private_path));
    assert!(!refused_overwrite.status.success());
    assert!(!refused_overwrite
        .stderr
        .windows(11)
        .any(|w| w == b"PRIVATE KEY"));

    let public = run(cli()
        .arg("public-key")
        .arg("--private-key")
        .arg(&private_path)
        .arg("--output")
        .arg(&public_path));
    assert!(public.status.success(), "{:?}", public.stderr);
    let public_json: Value = serde_json::from_slice(&public.stdout).unwrap();
    assert_eq!(public_json["key_id"], generated_json["key_id"]);

    fs::write(&subject_path, RESULT).unwrap();
    let signing_key = read_private_key_file(&private_path).unwrap();
    let subject = SubjectArtifact::from_bytes(
        "urn:coop:result:cli-test",
        "application/vnd.coop.execution-result.v1+json",
        RESULT,
    )
    .unwrap();
    let receipt = json!({
        "event_chain": {
            "complete": true,
            "events": 1,
            "head": "a".repeat(64),
            "version": 1
        },
        "job_id": "cli-test",
        "outcome": "succeeded",
        "receipt_sha256": "2558ad4053748cee96a333d82a520e74f1f2ad7890e88bc5e0d76befefeab460",
        "version": 1
    });
    let envelope = create_attestation(
        "tenant-cli",
        "cli-test",
        &subject,
        receipt.clone(),
        &[&signing_key],
    )
    .unwrap();
    fs::write(&envelope_path, envelope.to_json_bytes().unwrap()).unwrap();

    let verified = run(cli()
        .arg("verify")
        .arg("--envelope")
        .arg(&envelope_path)
        .arg("--subject")
        .arg(&subject_path)
        .arg("--public-key")
        .arg(&public_path)
        .arg("--tenant")
        .arg("tenant-cli")
        .arg("--subject-name")
        .arg("urn:coop:result:cli-test")
        .arg("--media-type")
        .arg("application/vnd.coop.execution-result.v1+json"));
    assert!(verified.status.success(), "{:?}", verified.stderr);
    let output: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(output["verified"], true);
    assert_eq!(output["execution_id"], "cli-test");
    assert_eq!(output["tenant"], "tenant-cli");
    assert_eq!(output["subject_size_bytes"], RESULT.len());
    assert_eq!(output["outcome"], "succeeded");
    assert_eq!(output["event_chain_complete"], true);
    assert!(output.get("receipt").is_none());

    let wrong_tenant = run(cli()
        .arg("verify")
        .arg("--envelope")
        .arg(&envelope_path)
        .arg("--subject")
        .arg(&subject_path)
        .arg("--public-key")
        .arg(&public_path)
        .arg("--tenant")
        .arg("tenant-other"));
    assert!(!wrong_tenant.status.success());
    assert!(String::from_utf8_lossy(&wrong_tenant.stderr).contains("tenant"));

    let digest = ArtifactDigest::from_bytes(RESULT);
    let legacy_statement = serde_json::to_vec(&json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{
            "name": "urn:coop:result:cli-test",
            "digest": {"sha256": digest.sha256()},
            "mediaType": "application/vnd.coop.execution-result.v1+json",
        }],
        "predicateType": "https://github.com/sambai-dev/coop/blob/main/crates/coop-attestation/FORMAT.md#predicate-v1",
        "predicate": {
            "schemaVersion": 1,
            "executionId": "cli-test",
            "result": {
                "name": "urn:coop:result:cli-test",
                "mediaType": "application/vnd.coop.execution-result.v1+json",
                "sizeBytes": digest.size_bytes(),
                "sha256": digest.sha256(),
            },
            "receipt": receipt,
        },
    }))
    .unwrap();
    let legacy_pae = dsse_v1_pae(DSSE_PAYLOAD_TYPE, &legacy_statement).unwrap();
    let legacy_envelope = serde_json::to_vec(&json!({
        "payloadType": DSSE_PAYLOAD_TYPE,
        "payload": BASE64_STANDARD.encode(legacy_statement),
        "signatures": [{
            "keyid": generated_json["key_id"],
            "sig": BASE64_STANDARD.encode(signing_key.sign(&legacy_pae).to_bytes()),
        }],
    }))
    .unwrap();
    fs::write(&legacy_envelope_path, legacy_envelope).unwrap();
    let rejected_legacy = run(cli()
        .arg("verify")
        .arg("--envelope")
        .arg(&legacy_envelope_path)
        .arg("--subject")
        .arg(&subject_path)
        .arg("--public-key")
        .arg(&public_path)
        .arg("--tenant")
        .arg("tenant-cli"));
    assert!(!rejected_legacy.status.success());
    assert!(String::from_utf8_lossy(&rejected_legacy.stderr).contains("in-toto statement"));

    fs::write(&subject_path, b"tampered result\n").unwrap();
    let rejected = run(cli()
        .arg("verify")
        .arg("--envelope")
        .arg(&envelope_path)
        .arg("--subject")
        .arg(&subject_path)
        .arg("--public-key")
        .arg(&public_path));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("SHA-256"));

    let non_regular = run(cli()
        .arg("verify")
        .arg("--envelope")
        .arg(&envelope_path)
        .arg("--subject")
        .arg(temp.path())
        .arg("--public-key")
        .arg(&public_path));
    assert!(!non_regular.status.success());
    assert!(String::from_utf8_lossy(&non_regular.stderr).contains("regular file"));

    let zero_threshold = run(cli()
        .arg("verify")
        .arg("--envelope")
        .arg(&envelope_path)
        .arg("--subject")
        .arg(&subject_path)
        .arg("--public-key")
        .arg(&public_path)
        .arg("--threshold")
        .arg("0"));
    assert_eq!(zero_threshold.status.code(), Some(2));
}
