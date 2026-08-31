use base64::Engine as _;
use clap::{Args, Parser, Subcommand};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    Serve,
    Exit,
}

#[derive(Debug, Parser)]
#[command(
    name = "rookhold",
    version = crate::VERSION,
    about = "Run short Python, Node, and Bash jobs with hard limits and receipts"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Start the configured long-running service.
    Serve,
    /// Start a loopback-only, unisolated service for trusted local code.
    Dev,
    /// Run one job and wait for its result.
    Run(RunArgs),
    /// Check the configured service, credentials, runtimes, and actual isolation.
    Check,
    /// List recent jobs from the configured service.
    Jobs {
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=500))]
        limit: u16,
    },
    /// Show one job and its receipt.
    Show { job_id: String },
    /// Run the packaged offline receipt verifier.
    Verify {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    /// Run the packaged MCP adapter over standard input and output.
    Mcp,
    /// Configure an MCP host, backing up the existing file before changes.
    Setup {
        #[arg(value_parser = ["claude-code", "hermes", "opencode", "generic-mcp"])]
        host: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(value_parser = ["python", "node", "bash"])]
    language: String,
    /// Inline code, or a source file path.
    code: String,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=300))]
    wall_seconds: Option<u32>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(16..=4096))]
    mem_mb: Option<u32>,
    #[arg(long)]
    stdin: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    minimum_isolation: Option<String>,
    /// Add LOCAL_PATH as input/<filename>, or use LOCAL_PATH=input/custom-name.
    #[arg(long = "file")]
    files: Vec<String>,
    /// Return one file written under output/.
    #[arg(long = "output")]
    outputs: Vec<String>,
    /// Select a versioned runtime pack advertised by the service.
    #[arg(long)]
    runtime: Option<String>,
}

pub async fn dispatch() -> Result<Dispatch, String> {
    match Cli::parse().command {
        None | Some(CliCommand::Serve) => Ok(Dispatch::Serve),
        Some(CliCommand::Dev) => {
            configure_development_env()?;
            eprintln!("WARNING: isolation is none; this mode does not contain untrusted code.");
            eprintln!("For untrusted code, connect to a Linux Rookhold service using gVisor.");
            Ok(Dispatch::Serve)
        }
        Some(CliCommand::Run(args)) => {
            run_once(args).await?;
            Ok(Dispatch::Exit)
        }
        Some(CliCommand::Check) => {
            check().await?;
            Ok(Dispatch::Exit)
        }
        Some(CliCommand::Jobs { limit }) => {
            print_authenticated_json(&format!("/v1/jobs?limit={limit}")).await?;
            Ok(Dispatch::Exit)
        }
        Some(CliCommand::Show { job_id }) => {
            validate_job_id(&job_id)?;
            print_authenticated_json(&format!("/v1/jobs/{job_id}")).await?;
            Ok(Dispatch::Exit)
        }
        Some(CliCommand::Verify { arguments }) => {
            delegate("rookhold-verify", "verify", &arguments)?;
            Ok(Dispatch::Exit)
        }
        Some(CliCommand::Mcp) => {
            delegate("rookhold-cli", "mcp-server", &[])?;
            Ok(Dispatch::Exit)
        }
        Some(CliCommand::Setup { host, yes }) => {
            let mut arguments = vec![host];
            if yes {
                arguments.push("--yes".to_string());
            }
            delegate("rookhold-cli", "setup", &arguments)?;
            Ok(Dispatch::Exit)
        }
    }
}

fn configure_development_env() -> Result<(), String> {
    let root = std::env::current_dir()
        .map_err(|error| format!("could not read the current directory: {error}"))?
        .join(".rookhold")
        .join("dev");
    set_default_env("ROOKHOLD_ADDR", "127.0.0.1:7300");
    set_default_env("ROOKHOLD_SANDBOX", "off");
    set_default_env("ROOKHOLD_ENV", "development");
    set_default_env("ROOKHOLD_API_KEYS", "local:rookhold-dev-key");
    set_default_env(
        "ROOKHOLD_DB",
        &root.join("rookhold.sqlite").to_string_lossy(),
    );
    set_default_env(
        "ROOKHOLD_JOBS_ROOT",
        &root.join("state").join("jobs").to_string_lossy(),
    );
    Ok(())
}

fn set_default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

struct Endpoint {
    base_url: String,
    api_key: String,
    temporary: Option<TemporaryServer>,
}

impl Endpoint {
    async fn resolve() -> Result<Self, String> {
        let base_url = compatible_env("ROOKHOLD_BASE_URL", "COOP_BASE_URL")?;
        let api_key = compatible_env("ROOKHOLD_API_KEY", "COOP_API_KEY")?;
        match (base_url, api_key) {
            (Some(base_url), Some(api_key)) => Ok(Self {
                base_url: validate_base_url(&base_url)?,
                api_key,
                temporary: None,
            }),
            (Some(_), None) => {
                Err("ROOKHOLD_API_KEY is required for the configured service".into())
            }
            (None, Some(_)) => {
                Err("ROOKHOLD_BASE_URL is required when ROOKHOLD_API_KEY is set".into())
            }
            (None, None) => TemporaryServer::start().await,
        }
    }

    fn is_temporary(&self) -> bool {
        self.temporary.is_some()
    }
}

fn compatible_env(primary: &str, legacy: &str) -> Result<Option<String>, String> {
    let current = std::env::var(primary)
        .ok()
        .filter(|value| !value.is_empty());
    let old = std::env::var(legacy).ok().filter(|value| !value.is_empty());
    if current.is_some() && old.is_some() && current != old {
        return Err(format!("{primary} conflicts with {legacy}"));
    }
    Ok(current.or(old))
}

fn validate_base_url(raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| "ROOKHOLD_BASE_URL must be an absolute HTTP URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "ROOKHOLD_BASE_URL must be an absolute HTTP URL without credentials, query, or fragment"
                .to_string(),
        );
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

struct TemporaryServer {
    child: Child,
    root: PathBuf,
}

impl TemporaryServer {
    async fn start() -> Result<Endpoint, String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("could not reserve a loopback port: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("could not inspect the loopback port: {error}"))?
            .port();
        drop(listener);
        let nonce = uuid::Uuid::now_v7().to_string();
        let root = std::env::temp_dir().join(format!("rookhold-run-{nonce}"));
        std::fs::create_dir(&root)
            .map_err(|error| format!("could not create temporary state: {error}"))?;
        let executable = std::env::current_exe()
            .map_err(|error| format!("could not locate the Rookhold executable: {error}"))?;
        let api_key = format!("local-{}", uuid::Uuid::now_v7().simple());
        let mut child = Command::new(executable)
            .arg("serve")
            .env("ROOKHOLD_ADDR", format!("127.0.0.1:{port}"))
            .env("ROOKHOLD_API_KEYS", format!("local:{api_key}"))
            .env("ROOKHOLD_SANDBOX", "off")
            .env("ROOKHOLD_ENV", "development")
            .env("ROOKHOLD_DB", root.join("rookhold.sqlite"))
            .env("ROOKHOLD_JOBS_ROOT", root.join("state").join("jobs"))
            .env("ROOKHOLD_LOG_FORMAT", "compact")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start the temporary service: {error}"))?;
        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(|error| format!("could not build the local HTTP client: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("could not inspect the temporary service: {error}"))?
            {
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!(
                    "temporary service stopped during startup ({status})"
                ));
            }
            if client
                .get(format!("{base_url}/healthz"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(Endpoint {
                    base_url,
                    api_key,
                    temporary: Some(Self { child, root }),
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&root);
        Err("temporary service was not ready after 15 seconds".into())
    }
}

impl Drop for TemporaryServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rookhold-run-"))
        {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

async fn run_once(args: RunArgs) -> Result<(), String> {
    let endpoint = Endpoint::resolve().await?;
    let client = http_client()?;
    let capabilities = get_json(&client, &endpoint, "/v1/capabilities").await?;
    let configured_isolation = capabilities
        .pointer("/execution/isolation_class")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let minimum_isolation = args
        .minimum_isolation
        .as_deref()
        .unwrap_or(configured_isolation);
    let code = read_code(&args.code)?;
    let mut limits = serde_json::Map::new();
    if let Some(value) = args.wall_seconds {
        limits.insert("wall_seconds".into(), json!(value));
    }
    if let Some(value) = args.mem_mb {
        limits.insert("mem_mb".into(), json!(value));
    }
    let mut body = json!({
        "language": args.language,
        "code": code,
        "requirements": {"minimum_isolation": minimum_isolation},
    });
    if !limits.is_empty() {
        body["limits"] = Value::Object(limits);
    }
    if let Some(stdin) = args.stdin {
        body["stdin"] = json!(stdin);
    }
    if !args.files.is_empty() {
        body["files"] = Value::Array(encode_input_files(&args.files)?);
    }
    if !args.outputs.is_empty() {
        body["outputs"] = json!(args.outputs);
    }
    if let Some(runtime) = args.runtime {
        body["runtime"] = json!(runtime);
    }
    let response = client
        .post(format!("{}/v1/jobs", endpoint.base_url))
        .bearer_auth(&endpoint.api_key)
        .header("Idempotency-Key", uuid::Uuid::now_v7().to_string())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::to_vec(&body)
                .map_err(|error| format!("could not encode job request: {error}"))?,
        )
        .send()
        .await
        .map_err(|error| format!("job submission failed: {error}"))?;
    let submitted = checked_json(response).await?;
    let job_id = submitted
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "job submission did not return a job_id".to_string())?;
    validate_job_id(job_id)?;
    let wait_seconds = args.wall_seconds.unwrap_or(15).saturating_add(30).min(330);
    let streamed = if args.json {
        false
    } else {
        stream_job_output(&client, &endpoint, job_id, wait_seconds).await?
    };
    let result_response = client
        .get(format!(
            "{}/v1/jobs/{job_id}/result?wait_seconds={wait_seconds}",
            endpoint.base_url
        ))
        .bearer_auth(&endpoint.api_key)
        .timeout(Duration::from_secs(u64::from(wait_seconds) + 5))
        .send()
        .await
        .map_err(|error| format!("waiting for job {job_id} failed: {error}"))?;
    let result = checked_json(result_response).await?;
    let detail = get_json(&client, &endpoint, &format!("/v1/jobs/{job_id}")).await?;
    let receipt_path = save_receipt(job_id, detail.get("receipt"))?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "result": result,
                "receipt": detail.get("receipt"),
                "receipt_path": receipt_path,
            }))
            .map_err(|error| format!("could not encode output: {error}"))?
        );
    } else {
        print_human_result(
            &result,
            &detail,
            receipt_path.as_deref(),
            endpoint.is_temporary(),
            streamed,
        );
    }
    if result.get("status").and_then(Value::as_str) == Some("succeeded") {
        Ok(())
    } else {
        Err(format!(
            "job {job_id} finished with status {}",
            result
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ))
    }
}

async fn stream_job_output(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    job_id: &str,
    wait_seconds: u32,
) -> Result<bool, String> {
    use std::io::Write as _;

    let deadline = Instant::now() + Duration::from_secs(u64::from(wait_seconds) + 5);
    let mut after = 0_i64;
    let mut printed = false;
    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "job {job_id} did not finish within the caller timeout"
            ));
        }
        let replay = get_json(
            client,
            endpoint,
            &format!("/v1/jobs/{job_id}/replay?after={after}&limit=500"),
        )
        .await?;
        let events = replay
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| "event replay returned no events array".to_string())?;
        let mut terminal = false;
        for event in events {
            if let Some(seq) = event.get("seq").and_then(Value::as_i64) {
                after = after.max(seq);
            }
            let line = event.pointer("/data/line").and_then(Value::as_str);
            match (event.get("kind").and_then(Value::as_str), line) {
                (Some("stdout"), Some(line)) => {
                    println!("{line}");
                    printed = true;
                }
                (Some("stderr"), Some(line)) => {
                    eprintln!("{line}");
                    printed = true;
                }
                (Some("finished"), _) => terminal = true,
                _ => {}
            }
        }
        std::io::stdout()
            .flush()
            .map_err(|error| format!("could not flush live output: {error}"))?;
        std::io::stderr()
            .flush()
            .map_err(|error| format!("could not flush live error output: {error}"))?;
        if terminal {
            return Ok(printed);
        }
        if replay
            .get("next_cursor")
            .is_some_and(|value| !value.is_null())
        {
            continue;
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

fn read_code(value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if path.is_file() {
        return std::fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()));
    }
    if value.trim().is_empty() {
        return Err("code must not be empty".into());
    }
    Ok(value.to_string())
}

fn encode_input_files(values: &[String]) -> Result<Vec<Value>, String> {
    let mut total = 0_usize;
    values
        .iter()
        .map(|value| {
            let (local, remote) = match value.split_once('=') {
                Some((local, remote)) => (PathBuf::from(local), remote.to_string()),
                None => {
                    let local = PathBuf::from(value);
                    let name = local
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| format!("input file {value:?} has no safe file name"))?
                        .to_string();
                    (local, format!("input/{name}"))
                }
            };
            if !coop_types::validate_artifact_path(&remote, "input") {
                return Err(format!(
                    "input destination {remote:?} must be a safe path under input/"
                ));
            }
            let bytes = std::fs::read(&local)
                .map_err(|error| format!("could not read {}: {error}", local.display()))?;
            if bytes.len() > coop_types::MAX_INPUT_FILE_BYTES {
                return Err(format!(
                    "{} exceeds the {} byte per-file limit",
                    local.display(),
                    coop_types::MAX_INPUT_FILE_BYTES
                ));
            }
            total = total.saturating_add(bytes.len());
            if total > coop_types::MAX_INPUT_BYTES {
                return Err(format!(
                    "input files exceed the {} byte total limit",
                    coop_types::MAX_INPUT_BYTES
                ));
            }
            Ok(json!({
                "path": remote,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            }))
        })
        .collect()
}

fn validate_job_id(value: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| "job_id was not a UUID".to_string())
}

fn save_receipt(job_id: &str, receipt: Option<&Value>) -> Result<Option<String>, String> {
    let Some(receipt) = receipt.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let directory = std::env::current_dir()
        .map_err(|error| format!("could not read the current directory: {error}"))?
        .join(".rookhold")
        .join("runs")
        .join(job_id);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    let path = directory.join("receipt.json");
    let bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|error| format!("could not encode receipt: {error}"))?;
    std::fs::write(&path, bytes)
        .map_err(|error| format!("could not save {}: {error}", path.display()))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

fn print_human_result(
    result: &Value,
    detail: &Value,
    receipt_path: Option<&str>,
    local: bool,
    output_already_streamed: bool,
) {
    let stdout = result.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = result.get("stderr").and_then(Value::as_str).unwrap_or("");
    if !output_already_streamed && !stdout.is_empty() {
        print!("{stdout}");
        if !stdout.ends_with('\n') {
            println!();
        }
    }
    if !output_already_streamed && !stderr.is_empty() {
        eprint!("{stderr}");
        if !stderr.ends_with('\n') {
            eprintln!();
        }
    }
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let duration_ms = result
        .get("duration_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if status == "timed_out" {
        println!("Job terminated after {:.2}s", duration_ms as f64 / 1_000.0);
    }
    println!();
    println!("status       {status}");
    println!("duration     {duration_ms}ms");
    let receipt = detail.get("receipt").unwrap_or(&Value::Null);
    let networking = receipt
        .get("networking")
        .and_then(Value::as_str)
        .or_else(|| {
            detail
                .pointer("/execution_policy/networking")
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown");
    let isolation = receipt
        .get("isolation_class")
        .and_then(Value::as_str)
        .or_else(|| {
            detail
                .pointer("/execution_policy/isolation_class")
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown");
    println!("network      {networking}");
    println!("isolation    {isolation}");
    if let Some(wall_seconds) = receipt
        .pointer("/effective_limits/wall_seconds")
        .and_then(Value::as_u64)
    {
        let enforced = receipt
            .pointer("/limit_enforcement/wall_seconds")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        println!(
            "wall limit   {wall_seconds}s — {}",
            if enforced { "enforced" } else { "not enforced" }
        );
    }
    println!(
        "receipt      {}",
        receipt_path
            .map(|path| format!("saved to {path}"))
            .unwrap_or_else(|| "not available".to_string())
    );
    if let Some(artifacts) = result.get("artifacts").and_then(Value::as_array) {
        for artifact in artifacts {
            if let (Some(path), Some(size)) = (
                artifact.get("path").and_then(Value::as_str),
                artifact.get("size_bytes").and_then(Value::as_u64),
            ) {
                println!("artifact     {path} ({size} bytes)");
            }
        }
    }
    if local || isolation == "none" {
        println!();
        println!("WARNING: isolation is none; this run did not contain untrusted code.");
        println!("For untrusted code, connect to a Linux Rookhold service using gVisor.");
    }
}

async fn check() -> Result<(), String> {
    let endpoint = Endpoint::resolve().await?;
    let client = http_client()?;
    println!("OK    rookhold executable found");
    let capabilities = get_json(&client, &endpoint, "/v1/capabilities").await?;
    println!("OK    service reachable at {}", endpoint.base_url);
    let identity = get_json(&client, &endpoint, "/v1/whoami").await?;
    println!(
        "OK    credential accepted for tenant {}",
        identity
            .get("tenant")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    for language in ["python", "node", "bash"] {
        let available = capabilities
            .get("languages")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(language)));
        println!(
            "{}    {language} runtime {}",
            if available { "OK" } else { "WARN" },
            if available {
                "available"
            } else {
                "not available"
            }
        );
    }
    let isolation = capabilities
        .pointer("/execution/isolation_class")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    println!(
        "{}    isolation {isolation}",
        if isolation == "none" { "WARN" } else { "OK" }
    );
    if isolation == "none" {
        println!("WARN  this service does not contain untrusted code");
    }
    check_mcp(&endpoint).await?;
    Ok(())
}

async fn check_mcp(endpoint: &Endpoint) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let executable = sibling_executable("rookhold-cli")?;
    let mut child = match tokio::process::Command::new(&executable)
        .arg("mcp-server")
        .env("ROOKHOLD_BASE_URL", &endpoint.base_url)
        .env("ROOKHOLD_API_KEY", &endpoint.api_key)
        .env_remove("COOP_BASE_URL")
        .env_remove("COOP_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("WARN  packaged MCP command not found; service checks passed");
            return Ok(());
        }
        Err(error) => return Err(format!("could not start the MCP command: {error}")),
    };
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "MCP command did not expose standard input".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "MCP command did not expose standard output".to_string())?;
    let request = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
    );
    stdin
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("could not write the MCP test request: {error}"))?;
    stdin
        .shutdown()
        .await
        .map_err(|error| format!("could not finish the MCP test request: {error}"))?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let responses = tokio::time::timeout(Duration::from_secs(5), async {
        let mut values = Vec::new();
        while let Some(line) = lines.next_line().await? {
            let value = serde_json::from_str::<Value>(&line)
                .map_err(|error| std::io::Error::other(format!("invalid MCP JSON: {error}")))?;
            values.push(value);
            if values
                .iter()
                .any(|value| value.get("id") == Some(&json!(2)))
            {
                break;
            }
        }
        Ok::<Vec<Value>, std::io::Error>(values)
    })
    .await
    .map_err(|_| "MCP initialization exceeded 5 seconds".to_string())?
    .map_err(|error| format!("MCP initialization failed: {error}"))?;
    let tools = responses
        .iter()
        .find(|value| value.get("id") == Some(&json!(2)))
        .and_then(|value| value.pointer("/result/tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP tool listing returned no tools".to_string())?;
    let _ = child.kill().await;
    println!(
        "OK    MCP initialization succeeded; {} tools exposed",
        tools.len()
    );
    Ok(())
}

async fn print_authenticated_json(path: &str) -> Result<(), String> {
    let endpoint = configured_endpoint()?;
    let value = get_json(&http_client()?, &endpoint, path).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value)
            .map_err(|error| format!("could not encode response: {error}"))?
    );
    Ok(())
}

fn configured_endpoint() -> Result<Endpoint, String> {
    let base_url = compatible_env("ROOKHOLD_BASE_URL", "COOP_BASE_URL")?
        .ok_or_else(|| "ROOKHOLD_BASE_URL is required".to_string())?;
    let api_key = compatible_env("ROOKHOLD_API_KEY", "COOP_API_KEY")?
        .ok_or_else(|| "ROOKHOLD_API_KEY is required".to_string())?;
    Ok(Endpoint {
        base_url: validate_base_url(&base_url)?,
        api_key,
        temporary: None,
    })
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("could not build the HTTP client: {error}"))
}

async fn get_json(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    path: &str,
) -> Result<Value, String> {
    let response = client
        .get(format!("{}{}", endpoint.base_url, path))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .map_err(|error| format!("request to {path} failed: {error}"))?;
    checked_json(response).await
}

async fn checked_json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("could not read the HTTP response: {error}"))?;
    if !status.is_success() {
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        return Err(format!("HTTP {}: {message}", status.as_u16()));
    }
    if status == StatusCode::NO_CONTENT || bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("server returned invalid JSON: {error}"))
}

fn delegate(program: &str, subcommand: &str, arguments: &[String]) -> Result<(), String> {
    let executable = sibling_executable(program)?;
    let status = Command::new(&executable)
        .arg(subcommand)
        .args(arguments)
        .status()
        .map_err(|error| format!("could not start {}: {error}", executable.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {status}", executable.display()))
    }
}

fn sibling_executable(name: &str) -> Result<PathBuf, String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("could not locate the Rookhold executable: {error}"))?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let sibling = current.with_file_name(format!("{name}{suffix}"));
    if sibling.is_file() {
        return Ok(sibling);
    }
    Ok(PathBuf::from(format!("{name}{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_and_source_files_are_both_supported() {
        assert_eq!(read_code("print(42)").unwrap(), "print(42)");
        let path = std::env::temp_dir().join(format!("rookhold-cli-{}.py", uuid::Uuid::now_v7()));
        std::fs::write(&path, "print(7)").unwrap();
        assert_eq!(read_code(path.to_str().unwrap()).unwrap(), "print(7)");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn receipt_paths_require_uuid_job_ids() {
        assert!(validate_job_id(&uuid::Uuid::now_v7().to_string()).is_ok());
        assert!(validate_job_id("../outside").is_err());
    }

    #[test]
    fn configured_base_urls_reject_credential_and_redirect_confusion() {
        assert_eq!(
            validate_base_url("https://example.test/prefix/").unwrap(),
            "https://example.test/prefix"
        );
        for invalid in [
            "file:///tmp/rookhold",
            "https://key@example.test",
            "https://example.test?next=https://other.test",
            "https://example.test/#fragment",
        ] {
            assert!(validate_base_url(invalid).is_err(), "{invalid}");
        }
    }
}
