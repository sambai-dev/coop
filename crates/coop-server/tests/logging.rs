use std::process::Command;

#[test]
fn json_mode_fatal_startup_is_ndjson_without_plaintext_suffix() {
    let output = Command::new(env!("CARGO_BIN_EXE_coop"))
        .env("COOP_ENV", "production")
        .env("COOP_LOG_FORMAT", "json")
        .env("RUST_LOG", "info")
        .env_remove("COOP_API_KEYS")
        .output()
        .expect("run coop with deliberately invalid production config");
    assert!(
        !output.status.success(),
        "invalid production config must fail"
    );

    let combined = format!(
        "{}{}",
        String::from_utf8(output.stdout).expect("JSON stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("JSON stderr is UTF-8")
    );
    let mut records = 0_usize;
    for line in combined.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("non-JSON log {line:?}: {error}"));
        assert!(
            value.is_object(),
            "log record must be a JSON object: {value}"
        );
        records += 1;
    }
    assert!(
        records > 0,
        "fatal startup must emit a structured diagnosis"
    );
    assert!(combined.contains("Rookhold terminated"), "{combined}");
    assert!(
        !combined.contains("\nerror:"),
        "plaintext fatal suffix: {combined}"
    );
}
