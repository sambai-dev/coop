#![cfg(feature = "cli")]

use coop_attestation::{create_attestation, read_private_key_file, SubjectArtifact};
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
    let envelope = create_attestation(
        "cli-test",
        &subject,
        json!({
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
        }),
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
        .arg("--subject-name")
        .arg("urn:coop:result:cli-test")
        .arg("--media-type")
        .arg("application/vnd.coop.execution-result.v1+json"));
    assert!(verified.status.success(), "{:?}", verified.stderr);
    let output: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(output["verified"], true);
    assert_eq!(output["execution_id"], "cli-test");
    assert_eq!(output["subject_size_bytes"], RESULT.len());
    assert_eq!(output["outcome"], "succeeded");
    assert_eq!(output["event_chain_complete"], true);
    assert!(output.get("receipt").is_none());

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
