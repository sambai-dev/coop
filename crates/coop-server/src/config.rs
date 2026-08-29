use std::collections::HashMap;
use std::path::{Component, Path};

pub const DEV_DEFAULT_API_KEY: &str = "local:coop-dev-key";
pub const DEFAULT_TENANT_QUEUE_CAPACITY: usize = 64;
pub const DEFAULT_MAX_JOB_MEM_MB: u32 = 1024;
pub const DEFAULT_MEMORY_BUDGET_MB: u32 = 4096;
pub const DEFAULT_STORAGE_GLOBAL_MB: u64 = 16 * 1024;
pub const DEFAULT_STORAGE_TENANT_MB: u64 = 4 * 1024;
pub const DEFAULT_STORAGE_FREE_RESERVE_MB: u64 = 1024;

#[derive(Clone)]
pub struct Config {
    pub addr: String,
    pub db_path: String,
    pub api_keys: HashMap<String, String>,
    /// Optional, separately scoped bearer credential for the global operator
    /// metrics endpoint. It is never accepted by tenant API middleware.
    pub metrics_token: Option<String>,
    pub credentials: crate::auth::CredentialStore,
    pub jwt: Option<crate::auth::JwtConfig>,
    pub workers: usize,
    pub tenant_concurrency: usize,
    pub tenant_queue_capacity: usize,
    pub rate_per_min: u32,
    pub max_job_mem_mb: u32,
    pub memory_budget_mb: u32,
    pub storage_global_mb: u64,
    pub storage_tenant_mb: u64,
    pub storage_free_reserve_mb: u64,
    pub sandbox: String,
    pub jobs_root: String,
    /// A private, purpose-built root filesystem used by the namespace
    /// executor. Host `/` is never an acceptable sandbox root.
    pub rootfs: Option<String>,
    /// Dedicated single-threaded bootstrap executable for namespace setup.
    pub sandbox_helper: Option<String>,
    /// Absolute path to the reviewed, operator-installed runsc binary.
    pub gvisor_runsc: Option<String>,
    /// Operator-generated content digest of the immutable trusted rootfs.
    pub gvisor_rootfs_sha256: Option<String>,
    /// gVisor syscall interception platform: systrap in a VM, or KVM on a
    /// suitably isolated bare-metal host.
    pub gvisor_platform: String,
    /// Non-root identity used by the OCI init and workload inside gVisor.
    pub gvisor_uid: u32,
    pub gvisor_gid: u32,
    /// Whether production policy is active. Kept in the parsed configuration
    /// so embedded servers cannot accidentally use the caller process' env.
    pub production: bool,
    /// Conspicuous acknowledgement required for the unisolated executor in
    /// production. This is deliberately separate from `COOP_SANDBOX=off`.
    pub unsafe_allow_naive: bool,
    pub unsafe_allow_public_dev: bool,
    pub python_bin: Option<String>,
    pub node_bin: Option<String>,
    pub bash_bin: Option<String>,
    /// F-009 retention: delete terminal jobs (and their events) older than
    /// this many hours. 0 disables sweeping entirely.
    pub retention_hours: u64,
    /// Seconds between retention sweeps.
    pub sweep_interval_secs: u64,
    /// F-005: install a seccomp-BPF syscall allowlist in sandboxed jobs
    /// (namespace backend only). Default on; `COOP_SECCOMP=off` disables.
    pub seccomp: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("addr", &self.addr)
            .field("db_path", &self.db_path)
            .field(
                "api_keys",
                &format!("{} key(s), redacted", self.api_keys.len()),
            )
            .field(
                "metrics_token",
                &self.metrics_token.as_ref().map(|_| "configured, redacted"),
            )
            .field("credentials", &self.credentials)
            .field("jwt", &self.jwt)
            .field("workers", &self.workers)
            .field("tenant_concurrency", &self.tenant_concurrency)
            .field("tenant_queue_capacity", &self.tenant_queue_capacity)
            .field("rate_per_min", &self.rate_per_min)
            .field("max_job_mem_mb", &self.max_job_mem_mb)
            .field("memory_budget_mb", &self.memory_budget_mb)
            .field("storage_global_mb", &self.storage_global_mb)
            .field("storage_tenant_mb", &self.storage_tenant_mb)
            .field("storage_free_reserve_mb", &self.storage_free_reserve_mb)
            .field("sandbox", &self.sandbox)
            .field("jobs_root", &self.jobs_root)
            .field("rootfs", &self.rootfs)
            .field("sandbox_helper", &self.sandbox_helper)
            .field("gvisor_runsc", &self.gvisor_runsc)
            .field("gvisor_rootfs_sha256", &self.gvisor_rootfs_sha256)
            .field("gvisor_platform", &self.gvisor_platform)
            .field("gvisor_uid", &self.gvisor_uid)
            .field("gvisor_gid", &self.gvisor_gid)
            .field("production", &self.production)
            .field("unsafe_allow_naive", &self.unsafe_allow_naive)
            .field("unsafe_allow_public_dev", &self.unsafe_allow_public_dev)
            .field("python_bin", &self.python_bin)
            .field("node_bin", &self.node_bin)
            .field("bash_bin", &self.bash_bin)
            .finish()
    }
}

fn env_or(getenv: &dyn Fn(&str) -> Option<String>, key: &str, default: &str) -> String {
    getenv(key)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn parse_number<T>(
    getenv: &dyn Fn(&str) -> Option<String>,
    key: &str,
    default: &str,
    min: T,
    max: T,
) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + Copy + std::fmt::Display,
{
    let raw = env_or(getenv, key, default);
    let value = raw
        .parse::<T>()
        .map_err(|_| format!("{key} must be a base-10 integer, got {raw:?}"))?;
    if value < min || value > max {
        return Err(format!(
            "{key} must be between {min} and {max}, got {value}"
        ));
    }
    Ok(value)
}

fn env_true(getenv: &dyn Fn(&str) -> Option<String>, key: &str) -> bool {
    matches!(
        getenv(key)
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn listener_is_loopback(addr: &str) -> bool {
    let addr = addr.trim();
    if let Ok(socket) = addr.parse::<std::net::SocketAddr>() {
        return socket.ip().is_loopback();
    }
    false
}

/// Reject paths for which a permissions call could affect a broad or
/// redirected part of the host. The executor creates children beneath this
/// directory, so it must be an absolute, dedicated, non-symlink path.
pub fn validate_jobs_root(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("COOP_JOBS_ROOT must be an absolute path".to_string());
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("COOP_JOBS_ROOT must not contain '.' or '..' components".to_string());
    }

    let normal_components = path
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .count();
    if normal_components < 2 {
        return Err(format!(
            "COOP_JOBS_ROOT={} is too broad; choose a dedicated directory such as /var/lib/coop/jobs",
            path.display()
        ));
    }

    // Known broad directories are dangerous even though some have two path
    // components on Windows (for example C:\\Users).
    let lexical: std::path::PathBuf = path.components().collect();
    let normalized = lexical
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let normalized = normalized.trim_end_matches('/');
    let broad = [
        "/applications",
        "/library",
        "/network",
        "/system",
        "/users",
        "/volumes",
        "/app",
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/tmp",
        "/var",
        "/var/cache",
        "/var/lib",
        "/var/log",
        "/var/run",
        "/var/spool",
        "/var/tmp",
        "/usr",
        "/usr/bin",
        "/usr/include",
        "/usr/lib",
        "/usr/lib64",
        "/usr/local",
        "/usr/local/bin",
        "/usr/local/etc",
        "/usr/local/lib",
        "/usr/local/sbin",
        "/usr/sbin",
        "/usr/share",
        "/opt",
        "/home",
        "/lib",
        "/lib64",
        "/media",
        "/mnt",
        "/proc",
        "/private",
        "/private/etc",
        "/private/tmp",
        "/private/var",
        "/private/var/lib",
        "/private/var/tmp",
        "/root",
        "/run",
        "/sbin",
        "/srv",
        "/sys",
        "/workspace",
        "c:/windows",
        "c:/windows/system32",
        "c:/program files",
        "c:/program files (x86)",
        "c:/programdata",
        "c:/users",
    ];
    if broad.contains(&normalized) {
        return Err(format!(
            "COOP_JOBS_ROOT={} is a shared system directory; choose a dedicated child",
            path.display()
        ));
    }
    let mut dynamic_forbidden = vec![std::env::temp_dir()];
    if let Ok(current) = std::env::current_dir() {
        dynamic_forbidden.push(current);
    }
    for variable in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(variable) {
            dynamic_forbidden.push(value.into());
        }
    }
    if dynamic_forbidden.iter().any(|candidate| {
        candidate
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .eq_ignore_ascii_case(normalized)
    }) {
        return Err(format!(
            "COOP_JOBS_ROOT={} must not be a home, temporary, or current working directory",
            path.display()
        ));
    }

    validate_existing_jobs_root_chain(path, false)
}

/// Validate, create, lock down, and revalidate the jobs directory. Strict
/// mode is for Linux production/namespace execution, where every existing
/// component must be root-owned and immune to group/world path replacement.
pub fn prepare_jobs_root(path: &Path, strict: bool) -> Result<(), String> {
    validate_jobs_root(path)?;
    validate_existing_jobs_root_chain(path, strict)?;
    if std::fs::symlink_metadata(path).is_ok() {
        require_existing_jobs_root_private(path)?;
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| format!("COOP_JOBS_ROOT {} has no dedicated parent", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut parents = std::fs::DirBuilder::new();
            parents.recursive(true).mode(0o700);
            parents.create(parent).map_err(|error| {
                format!(
                    "failed to create COOP_JOBS_ROOT parent {}: {error}",
                    parent.display()
                )
            })?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create COOP_JOBS_ROOT parent {}: {error}",
                parent.display()
            )
        })?;

        // Validate the freshly completed parent chain before the atomic leaf
        // create. Existing shared directories are never chmodded: if another
        // creator wins this race, it must already satisfy the private-leaf
        // invariant or startup fails without mutating it.
        validate_existing_jobs_root_chain(parent, strict)?;
        #[cfg(unix)]
        let create_result = {
            use std::os::unix::fs::DirBuilderExt;
            let mut leaf = std::fs::DirBuilder::new();
            leaf.mode(0o700).create(path)
        };
        #[cfg(not(unix))]
        let create_result = std::fs::create_dir(path);
        match create_result {
            Ok(()) => coop_exec::owner_only_dir(path).map_err(|error| {
                format!(
                    "failed to lock down newly created COOP_JOBS_ROOT {}: {error}",
                    path.display()
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                require_existing_jobs_root_private(path)?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to create COOP_JOBS_ROOT {}: {error}",
                    path.display()
                ));
            }
        }
    }
    validate_existing_jobs_root_chain(path, strict)
}

fn require_existing_jobs_root_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|error| {
                format!(
                    "cannot inspect existing COOP_JOBS_ROOT {}: {error}",
                    path.display()
                )
            })?
            .permissions()
            .mode()
            & 0o7777;
        if mode != 0o700 {
            return Err(format!(
                "existing COOP_JOBS_ROOT {} must already have mode 0700; refusing to chmod a potentially shared directory (found {mode:04o})",
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn validate_existing_jobs_root_chain(path: &Path, strict: bool) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    let _ = strict;
    for ancestor in path.ancestors() {
        let metadata = match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "cannot inspect COOP_JOBS_ROOT ancestor {}: {error}",
                    ancestor.display()
                ))
            }
        };
        if metadata.file_type().is_symlink() {
            #[cfg(target_os = "macos")]
            if is_trusted_macos_system_alias(ancestor) {
                continue;
            }
            return Err(format!(
                "COOP_JOBS_ROOT must not traverse a symlink: {}",
                ancestor.display()
            ));
        }
        if !metadata.is_dir() {
            return Err(format!(
                "COOP_JOBS_ROOT ancestor {} is not a directory",
                ancestor.display()
            ));
        }
        #[cfg(target_os = "linux")]
        if strict {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
                return Err(format!(
                    "COOP_JOBS_ROOT strict mode requires root-owned, non-group/world-writable components; {} is insecure",
                    ancestor.display()
                ));
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(format!(
                    "COOP_JOBS_ROOT must not traverse a junction or reparse point: {}",
                    ancestor.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_trusted_macos_system_alias(path: &Path) -> bool {
    let expected = match path.to_str() {
        Some("/var") => "/private/var",
        Some("/tmp") => "/private/tmp",
        Some("/etc") => "/private/etc",
        _ => return false,
    };
    path.canonicalize()
        .is_ok_and(|target| target == Path::new(expected))
}

fn default_jobs_root() -> String {
    if cfg!(target_os = "linux") {
        "/var/lib/coop/jobs".to_string()
    } else {
        std::env::temp_dir()
            .join("coop-jobs")
            .to_string_lossy()
            .into_owned()
    }
}

fn default_sandbox_helper() -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "coop-sandbox-init.exe"
    } else {
        "coop-sandbox-init"
    };
    let candidate = executable.parent()?.join(name);
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

fn is_production_env(value: Option<String>) -> bool {
    matches!(
        value
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("prod" | "production" | "release")
    )
}

/// True when the process should be treated as a production deployment
/// (COOP_ENV or NODE_ENV says so; the Docker image sets COOP_ENV=production).
pub fn is_production() -> bool {
    is_production_env(std::env::var("COOP_ENV").ok())
        || is_production_env(std::env::var("NODE_ENV").ok())
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_sources(&|k| std::env::var(k).ok(), is_production())
    }

    /// Build config from an explicit env-source + production flag (unit-testable core).
    pub fn from_sources(
        getenv: &dyn Fn(&str) -> Option<String>,
        production: bool,
    ) -> Result<Self, String> {
        let credentials_path = getenv("COOP_CREDENTIALS_FILE")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let pepper_path = getenv("COOP_CREDENTIAL_PEPPER_FILE")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let credentials = match (credentials_path.as_deref(), pepper_path.as_deref()) {
            (Some(credentials), Some(pepper)) => crate::auth::CredentialStore::load(
                Path::new(credentials),
                Path::new(pepper),
                production,
            )?,
            (None, None) => crate::auth::CredentialStore::default(),
            _ => return Err(
                "COOP_CREDENTIALS_FILE and COOP_CREDENTIAL_PEPPER_FILE must be configured together"
                    .to_string(),
            ),
        };

        let oidc_issuer = getenv("COOP_OIDC_ISSUER")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let oidc_audience = getenv("COOP_OIDC_AUDIENCE")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let oidc_jwks = getenv("COOP_OIDC_JWKS_URL")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let oidc_tenant_map = getenv("COOP_OIDC_TENANT_MAP")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let optional_oidc_values = [
            getenv("COOP_OIDC_TENANT_CLAIM"),
            getenv("COOP_OIDC_ALGORITHMS"),
            getenv("COOP_OIDC_JWKS_TTL_SECONDS"),
            getenv("COOP_OIDC_MAX_TOKEN_AGE_SECONDS"),
        ];
        let oidc_requested = oidc_issuer.is_some()
            || oidc_audience.is_some()
            || oidc_jwks.is_some()
            || oidc_tenant_map.is_some()
            || optional_oidc_values
                .iter()
                .any(|value| value.as_ref().is_some_and(|value| !value.trim().is_empty()));
        let jwt = if oidc_requested {
            let issuer = oidc_issuer.as_deref().ok_or_else(|| {
                "COOP_OIDC_ISSUER is required when OIDC authentication is configured".to_string()
            })?;
            let audience = oidc_audience.as_deref().ok_or_else(|| {
                "COOP_OIDC_AUDIENCE is required when OIDC authentication is configured".to_string()
            })?;
            let jwks = oidc_jwks.as_deref().ok_or_else(|| {
                "COOP_OIDC_JWKS_URL is required when OIDC authentication is configured".to_string()
            })?;
            let tenant_map = oidc_tenant_map.as_deref().ok_or_else(|| {
                "COOP_OIDC_TENANT_MAP is required when OIDC authentication is configured"
                    .to_string()
            })?;
            Some(crate::auth::JwtConfig::parse(
                issuer,
                audience,
                jwks,
                &env_or(getenv, "COOP_OIDC_TENANT_CLAIM", "tenant_id"),
                tenant_map,
                &env_or(getenv, "COOP_OIDC_ALGORITHMS", "RS256,ES256,EdDSA"),
                parse_number(
                    getenv,
                    "COOP_OIDC_JWKS_TTL_SECONDS",
                    "300",
                    60_u64,
                    3600_u64,
                )?,
                parse_number(
                    getenv,
                    "COOP_OIDC_MAX_TOKEN_AGE_SECONDS",
                    "3600",
                    60_u64,
                    86_400_u64,
                )?,
            )?)
        } else {
            None
        };

        let mut api_keys = HashMap::new();
        let raw = getenv("COOP_API_KEYS").filter(|v| !v.trim().is_empty());
        let raw = match raw {
            Some(raw) => Some(raw),
            None if !credentials.is_empty() || jwt.is_some() => None,
            None if production => {
                return Err(
                    "configure COOP_CREDENTIALS_FILE with COOP_CREDENTIAL_PEPPER_FILE or provide \
                     legacy COOP_API_KEYS; refusing to start production without credentials"
                        .to_string(),
                );
            }
            None => {
                tracing::warn!(
                    "SECURITY: no COOP_API_KEYS configured — falling back to the PUBLIC development \
                     default key '{DEV_DEFAULT_API_KEY}'. Anyone who can reach this server can run \
                     code on it. Set COOP_API_KEYS before exposing coop beyond localhost."
                );
                Some(DEV_DEFAULT_API_KEY.to_string())
            }
        };
        if let Some(raw) = raw {
            for entry in raw.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let (tenant, key) = match entry.split_once(':') {
                    Some((tenant, key)) => (tenant.trim(), key.trim()),
                    None if !production => ("local", entry),
                    None => {
                        return Err(
                            "each production COOP_API_KEYS entry must use tenant:key syntax"
                                .to_string(),
                        )
                    }
                };
                if tenant.is_empty() {
                    return Err("COOP_API_KEYS contains a blank tenant".to_string());
                }
                if key.is_empty() {
                    return Err(format!(
                        "COOP_API_KEYS contains a blank key for tenant {tenant:?}"
                    ));
                }
                if production && (key == "coop-dev-key" || key.len() < 16) {
                    return Err(format!(
                        "production API key for tenant {tenant:?} is public or too short (minimum 16 characters)"
                    ));
                }
                if api_keys
                    .insert(key.to_string(), tenant.to_string())
                    .is_some()
                {
                    return Err("COOP_API_KEYS contains a duplicate key".to_string());
                }
            }
        }

        if api_keys.is_empty() && credentials.is_empty() && jwt.is_none() {
            return Err(
                "credential configuration did not contain any usable credentials".to_string(),
            );
        }
        if production && !api_keys.is_empty() {
            tracing::warn!(
                "SECURITY: legacy COOP_API_KEYS are enabled in production; migrate to the indexed \
                 peppered COOP_CREDENTIALS_FILE format"
            );
        }

        let metrics_token = getenv("COOP_METRICS_TOKEN")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if metrics_token.as_ref().is_some_and(|token| token.len() < 16) {
            return Err(
                "COOP_METRICS_TOKEN must contain at least 16 characters when configured"
                    .to_string(),
            );
        }
        if metrics_token
            .as_ref()
            .is_some_and(|token| api_keys.contains_key(token))
        {
            return Err(
                "COOP_METRICS_TOKEN must be different from every tenant API key".to_string(),
            );
        }

        let sandbox = env_or(getenv, "COOP_SANDBOX", "auto");
        let unsafe_allow_naive = env_true(getenv, "COOP_UNSAFE_ALLOW_NAIVE");
        let unsafe_allow_public_dev = env_true(getenv, "COOP_UNSAFE_ALLOW_PUBLIC_DEV");
        let seccomp = !matches!(
            env_or(getenv, "COOP_SECCOMP", "auto")
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "off" | "none" | "disabled" | "false" | "0"
        );
        if production && !seccomp {
            return Err("COOP_SECCOMP cannot be disabled in production".to_string());
        }

        let tenant_queue_capacity = parse_number(
            getenv,
            "COOP_TENANT_QUEUE_CAPACITY",
            &DEFAULT_TENANT_QUEUE_CAPACITY.to_string(),
            1usize,
            crate::QUEUE_CAPACITY,
        )?;
        let max_job_mem_mb = parse_number(
            getenv,
            "COOP_MAX_JOB_MEM_MB",
            &DEFAULT_MAX_JOB_MEM_MB.to_string(),
            16u32,
            coop_types::MEM_MAX_MB,
        )?;
        let memory_budget_mb = parse_number(
            getenv,
            "COOP_MEMORY_BUDGET_MB",
            &DEFAULT_MEMORY_BUDGET_MB.to_string(),
            16u32,
            1_048_576u32,
        )?;
        if max_job_mem_mb > memory_budget_mb {
            return Err(format!(
                "COOP_MAX_JOB_MEM_MB ({max_job_mem_mb}) must not exceed COOP_MEMORY_BUDGET_MB ({memory_budget_mb})"
            ));
        }
        let storage_global_mb = parse_number(
            getenv,
            "COOP_STORAGE_GLOBAL_MB",
            &DEFAULT_STORAGE_GLOBAL_MB.to_string(),
            128u64,
            1_048_576u64,
        )?;
        let storage_tenant_mb = parse_number(
            getenv,
            "COOP_STORAGE_TENANT_MB",
            &DEFAULT_STORAGE_TENANT_MB.to_string(),
            64u64,
            1_048_576u64,
        )?;
        if storage_tenant_mb > storage_global_mb {
            return Err(format!(
                "COOP_STORAGE_TENANT_MB ({storage_tenant_mb}) must not exceed COOP_STORAGE_GLOBAL_MB ({storage_global_mb})"
            ));
        }
        let storage_free_reserve_mb = parse_number(
            getenv,
            "COOP_STORAGE_FREE_RESERVE_MB",
            &DEFAULT_STORAGE_FREE_RESERVE_MB.to_string(),
            0u64,
            1_048_576u64,
        )?;

        let config = Self {
            addr: env_or(getenv, "COOP_ADDR", "127.0.0.1:7300"),
            db_path: env_or(getenv, "COOP_DB", "coop.db"),
            api_keys,
            metrics_token,
            credentials,
            jwt,
            workers: parse_number(getenv, "COOP_WORKERS", "4", 1usize, 256usize)?,
            tenant_concurrency: parse_number(
                getenv,
                "COOP_TENANT_CONCURRENCY",
                "2",
                1usize,
                256usize,
            )?,
            tenant_queue_capacity,
            rate_per_min: parse_number(getenv, "COOP_RATE_PER_MIN", "120", 1u32, 1_000_000u32)?,
            max_job_mem_mb,
            memory_budget_mb,
            storage_global_mb,
            storage_tenant_mb,
            storage_free_reserve_mb,
            sandbox,
            jobs_root: env_or(getenv, "COOP_JOBS_ROOT", &default_jobs_root()),
            rootfs: getenv("COOP_ROOTFS")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            sandbox_helper: getenv("COOP_SANDBOX_HELPER")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .or_else(default_sandbox_helper),
            gvisor_runsc: getenv("COOP_GVISOR_RUNSC")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            gvisor_rootfs_sha256: getenv("COOP_GVISOR_ROOTFS_SHA256")
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
            gvisor_platform: env_or(getenv, "COOP_GVISOR_PLATFORM", "systrap"),
            gvisor_uid: parse_number(getenv, "COOP_GVISOR_UID", "65534", 1u32, 4_294_967_294u32)?,
            gvisor_gid: parse_number(getenv, "COOP_GVISOR_GID", "65534", 1u32, 4_294_967_294u32)?,
            production,
            unsafe_allow_naive,
            unsafe_allow_public_dev,
            python_bin: getenv("COOP_PYTHON"),
            node_bin: getenv("COOP_NODE"),
            bash_bin: getenv("COOP_BASH"),
            retention_hours: parse_number(getenv, "COOP_RETENTION_HOURS", "168", 0u64, 87_600u64)?,
            sweep_interval_secs: parse_number(
                getenv,
                "COOP_SWEEP_INTERVAL_SECS",
                "3600",
                60u64,
                86_400u64,
            )?,
            seccomp,
        };
        config.validate_declared_listener_security()?;
        Ok(config)
    }

    pub fn validate_declared_listener_security(&self) -> Result<(), String> {
        if listener_is_loopback(&self.addr) || self.unsafe_allow_public_dev {
            return Ok(());
        }
        if self.api_keys.contains_key("coop-dev-key") {
            return Err(
                "a non-loopback COOP_ADDR cannot use the public development API key; configure COOP_API_KEYS or set COOP_UNSAFE_ALLOW_PUBLIC_DEV=true to acknowledge the unsafe development exposure"
                    .to_string(),
            );
        }
        if matches!(
            self.sandbox.trim().to_ascii_lowercase().as_str(),
            "off" | "none" | "naive"
        ) {
            return Err(
                "a non-loopback COOP_ADDR cannot use the unisolated subprocess backend unless COOP_UNSAFE_ALLOW_PUBLIC_DEV=true is set"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn validate_resolved_listener_security(
        &self,
        mode: coop_exec::SandboxMode,
    ) -> Result<(), String> {
        self.validate_declared_listener_security()?;
        if !listener_is_loopback(&self.addr)
            && mode == coop_exec::SandboxMode::Off
            && !self.unsafe_allow_public_dev
        {
            return Err(
                "COOP_SANDBOX=auto resolved to the unisolated subprocess backend on a non-loopback listener; configure namespace isolation or set COOP_UNSAFE_ALLOW_PUBLIC_DEV=true to acknowledge the unsafe development exposure"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn validate_bound_listener_security(
        &self,
        bound: std::net::SocketAddr,
        mode: coop_exec::SandboxMode,
    ) -> Result<(), String> {
        if bound.ip().is_loopback() || self.unsafe_allow_public_dev {
            return Ok(());
        }
        if self.api_keys.contains_key("coop-dev-key") || mode == coop_exec::SandboxMode::Off {
            return Err(format!(
                "listener resolved to non-loopback address {bound} with an unsafe development credential or executor; configure production keys and namespace isolation, or explicitly set COOP_UNSAFE_ALLOW_PUBLIC_DEV=true"
            ));
        }
        Ok(())
    }

    pub fn interpreter_override(&self, language: &str) -> Option<String> {
        match language {
            "python" => self.python_bin.clone(),
            "node" => self.node_bin.clone(),
            _ => self.bash_bin.clone(),
        }
    }

    pub fn clamp_limits(&self, limits: coop_types::Limits) -> coop_types::Limits {
        let mut limits = limits.clamped();
        limits.mem_mb = self.clamp_mem_mb(limits.mem_mb);
        limits
    }

    pub fn clamp_mem_mb(&self, mem_mb: u32) -> u32 {
        mem_mb
            .clamp(16, coop_types::MEM_MAX_MB)
            .min(self.max_job_mem_mb)
    }

    pub fn storage_limits(&self) -> coop_store::StorageLimits {
        let mib = 1024_u64 * 1024;
        coop_store::StorageLimits::new(
            self.storage_global_mb.saturating_mul(mib),
            self.storage_tenant_mb.saturating_mul(mib),
            self.storage_free_reserve_mb.saturating_mul(mib),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn prod_mode_without_api_keys_is_rejected() {
        let err = Config::from_sources(&source(&[]), true).unwrap_err();
        assert!(err.contains("COOP_API_KEYS"), "{err}");
    }

    #[test]
    fn empty_api_keys_value_counts_as_unset_in_prod() {
        assert!(Config::from_sources(&source(&[("COOP_API_KEYS", "  ")]), true).is_err());
    }

    #[test]
    fn dev_mode_without_api_keys_falls_back_to_dev_default() {
        let cfg = Config::from_sources(&source(&[]), false).expect("dev default applies");
        assert_eq!(
            cfg.api_keys.get("coop-dev-key").map(String::as_str),
            Some("local")
        );
        assert_eq!(cfg.api_keys.len(), 1);
    }

    #[test]
    fn prod_mode_with_explicit_keys_does_not_get_dev_default() {
        let cfg = Config::from_sources(
            &source(&[("COOP_API_KEYS", "acme:correct-horse-battery-staple")]),
            true,
        )
        .unwrap();
        assert_eq!(
            cfg.api_keys
                .get("correct-horse-battery-staple")
                .map(String::as_str),
            Some("acme")
        );
        assert!(!cfg.api_keys.contains_key("coop-dev-key"));
    }

    #[test]
    fn credentials_file_can_be_the_only_auth_source_and_requires_a_paired_pepper() {
        let root =
            std::env::temp_dir().join(format!("coop-config-credentials-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let credentials = root.join("credentials.json");
        let pepper = root.join("pepper");
        std::fs::write(&pepper, "11".repeat(32)).unwrap();
        std::fs::write(
            &credentials,
            serde_json::json!({
                "version":1,
                "credentials":[{
                    "key_id":"agent-a",
                    "tenant_id":"tenant-a",
                    "principal_id":"principal-a",
                    "digest_hmac_sha256":"22".repeat(32),
                    "scopes":["jobs:read"],
                    "created_at_ms":1
                }]
            })
            .to_string(),
        )
        .unwrap();
        let credential_path = credentials.to_string_lossy().into_owned();
        let pepper_path = pepper.to_string_lossy().into_owned();
        let cfg = Config::from_sources(
            &source(&[
                ("COOP_CREDENTIALS_FILE", credential_path.as_str()),
                ("COOP_CREDENTIAL_PEPPER_FILE", pepper_path.as_str()),
            ]),
            false,
        )
        .unwrap();
        assert!(cfg.api_keys.is_empty());
        assert_eq!(cfg.credentials.len(), 1);
        assert!(Config::from_sources(
            &source(&[("COOP_CREDENTIALS_FILE", credential_path.as_str())]),
            false
        )
        .is_err());
    }

    #[test]
    fn oidc_can_be_the_only_auth_source_but_partial_or_insecure_config_fails_closed() {
        let complete = [
            ("COOP_OIDC_ISSUER", "https://issuer.example"),
            ("COOP_OIDC_AUDIENCE", "https://coop.example"),
            ("COOP_OIDC_JWKS_URL", "https://issuer.example/jwks"),
            ("COOP_OIDC_TENANT_MAP", "external=internal"),
        ];
        let cfg = Config::from_sources(&source(&complete), false).unwrap();
        assert!(cfg.api_keys.is_empty());
        assert!(cfg.jwt.is_some());

        assert!(Config::from_sources(
            &source(&[("COOP_OIDC_ISSUER", "https://issuer.example")]),
            false
        )
        .is_err());
        let mut insecure = complete;
        insecure[2].1 = "http://issuer.example/jwks";
        assert!(Config::from_sources(&source(&insecure), false).is_err());
    }

    #[test]
    fn debug_output_never_contains_plaintext_keys() {
        let cfg = Config::from_sources(
            &source(&[
                ("COOP_API_KEYS", "acme:s3cr3t-value-that-is-long"),
                ("COOP_METRICS_TOKEN", "metrics-secret-value"),
            ]),
            true,
        )
        .unwrap();
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("s3cr3t-value-that-is-long"),
            "leaked: {rendered}"
        );
        assert!(
            !rendered.contains("metrics-secret-value"),
            "leaked: {rendered}"
        );
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn metrics_token_is_optional_but_must_be_strong_when_configured() {
        let cfg = Config::from_sources(&source(&[]), false).unwrap();
        assert!(cfg.metrics_token.is_none());

        let error = Config::from_sources(&source(&[("COOP_METRICS_TOKEN", "too-short")]), false)
            .unwrap_err();
        assert!(error.contains("COOP_METRICS_TOKEN"), "{error}");

        let cfg = Config::from_sources(
            &source(&[("COOP_METRICS_TOKEN", "separate-operator-secret")]),
            false,
        )
        .unwrap();
        assert_eq!(
            cfg.metrics_token.as_deref(),
            Some("separate-operator-secret")
        );

        let error = Config::from_sources(
            &source(&[
                ("COOP_API_KEYS", "tenant:shared-secret-value"),
                ("COOP_METRICS_TOKEN", "shared-secret-value"),
            ]),
            false,
        )
        .unwrap_err();
        assert!(error.contains("different"), "{error}");
    }

    #[test]
    fn blank_tenants_keys_and_public_production_keys_are_rejected() {
        for raw in [
            ":a-long-enough-secret",
            "acme:",
            "acme:coop-dev-key",
            "acme:too-short",
        ] {
            let err =
                Config::from_sources(&source(&[("COOP_API_KEYS", raw)]), true).expect_err(raw);
            assert!(
                err.contains("blank") || err.contains("short") || err.contains("public"),
                "{raw}: {err}"
            );
        }
    }

    #[test]
    fn invalid_numeric_configuration_is_not_silently_defaulted() {
        for (key, value) in [
            ("COOP_WORKERS", "many"),
            ("COOP_TENANT_CONCURRENCY", "0"),
            ("COOP_RATE_PER_MIN", "-1"),
            ("COOP_SWEEP_INTERVAL_SECS", "59"),
        ] {
            let err = Config::from_sources(&source(&[(key, value)]), false).expect_err(key);
            assert!(err.contains(key), "{key}: {err}");
        }
    }

    #[test]
    fn production_cannot_disable_seccomp() {
        let err = Config::from_sources(
            &source(&[
                ("COOP_API_KEYS", "acme:correct-horse-battery-staple"),
                ("COOP_SECCOMP", "off"),
            ]),
            true,
        )
        .unwrap_err();
        assert!(err.contains("COOP_SECCOMP"), "{err}");
    }

    #[test]
    fn jobs_root_must_be_absolute_and_dedicated() {
        assert!(validate_jobs_root(Path::new("relative/jobs")).is_err());
        let root = if cfg!(windows) {
            Path::new("C:\\")
        } else {
            Path::new("/")
        };
        assert!(validate_jobs_root(root).is_err());
        let dedicated =
            std::env::temp_dir().join(format!("coop-config-test-{}/jobs", uuid::Uuid::now_v7()));
        assert!(
            validate_jobs_root(&dedicated).is_ok(),
            "{}",
            dedicated.display()
        );
    }

    #[test]
    fn shared_system_leaf_is_rejected_before_any_permission_change() {
        let configured = if cfg!(windows) {
            Path::new("C:\\Program Files")
        } else {
            Path::new("/usr/local")
        };
        let shared = if cfg!(unix) {
            Path::new("/usr/bin")
        } else {
            configured
        };
        #[cfg(unix)]
        let before = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(shared)
                .expect("standard shared system directory exists")
                .permissions()
                .mode()
                & 0o7777
        };

        let error =
            validate_jobs_root(configured).expect_err("shared system leaf is never dedicated");
        assert!(
            error.contains("shared system directory") || error.contains("too broad"),
            "{error}"
        );
        assert!(validate_jobs_root(shared).is_err());
        if cfg!(unix) {
            for broad in [
                "/opt",
                "/private/var/lib",
                "/usr/local",
                "/var/lib",
                "/workspace",
            ] {
                assert!(
                    validate_jobs_root(Path::new(broad)).is_err(),
                    "broad shared path was accepted: {broad}"
                );
            }
            let dotted = Path::new("/usr/./bin");
            let dotted_error = validate_jobs_root(dotted)
                .expect_err("dot components must not bypass the shared-root policy");
            assert!(
                dotted_error.contains("shared system directory"),
                "{dotted_error}"
            );
            let repeated = validate_jobs_root(Path::new("/usr//bin"))
                .expect_err("repeated separators must not bypass the shared-root policy");
            assert!(repeated.contains("shared system directory"), "{repeated}");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let after = std::fs::metadata(shared)
                .expect("validation did not alter the directory")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(after, before, "validation must be strictly non-mutating");
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_non_private_leaf_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::env::temp_dir().join(format!("coop-existing-root-{}", uuid::Uuid::now_v7()));
        let root = base.join("jobs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = prepare_jobs_root(&root, false)
            .expect_err("startup must not chmod an arbitrary existing directory");
        assert!(error.contains("must already have mode 0700"), "{error}");
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn development_jobs_root_under_os_temp_is_prepared_owner_only() {
        let root =
            std::env::temp_dir().join(format!("coop-config-prepare-{}/jobs", uuid::Uuid::now_v7()));
        prepare_jobs_root(&root, false).expect("prepare development jobs root");
        assert!(root.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn strict_jobs_root_rejects_shared_writable_ancestor() {
        let root =
            std::env::temp_dir().join(format!("coop-config-strict-{}/jobs", uuid::Uuid::now_v7()));
        let error = prepare_jobs_root(&root, true).unwrap_err();
        assert!(
            error.contains("root-owned") || error.contains("writable"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn jobs_root_rejects_dangling_symlink_component() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("coop-config-link-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&base).unwrap();
        let link = base.join("redirect");
        symlink(base.join("missing-target"), &link).unwrap();
        let error = validate_jobs_root(&link.join("jobs")).unwrap_err();
        assert!(error.contains("symlink"), "{error}");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_temp_jobs_root_accepts_trusted_system_aliases() {
        let root = std::env::temp_dir().join("coop-jobs");
        validate_jobs_root(&root).expect("macOS /var or /tmp system alias is trusted");
    }

    #[test]
    fn production_detection_matches_common_values() {
        for v in ["prod", "PRODUCTION", "production", "release", " Prod "] {
            assert!(is_production_env(Some(v.to_string())), "{v}");
        }
        for v in ["dev", "development", "", "staging"] {
            assert!(!is_production_env(Some(v.to_string())), "{v}");
        }
        assert!(!is_production_env(None));
    }

    #[test]
    fn non_loopback_development_listener_fails_closed_without_acknowledgement() {
        let error = Config::from_sources(&source(&[("COOP_ADDR", "0.0.0.0:7300")]), false)
            .expect_err("public fallback key must not bind publicly");
        assert!(error.contains("public development API key"), "{error}");

        let error = Config::from_sources(
            &source(&[
                ("COOP_ADDR", "0.0.0.0:7300"),
                ("COOP_API_KEYS", "local:coop-dev-key"),
                ("COOP_SANDBOX", "namespaces"),
            ]),
            false,
        )
        .expect_err("explicit public key must be detected semantically");
        assert!(error.contains("public development API key"), "{error}");

        let error = Config::from_sources(
            &source(&[
                ("COOP_ADDR", "[::]:7300"),
                ("COOP_API_KEYS", "tenant:a-long-development-key"),
                ("COOP_SANDBOX", "off"),
            ]),
            false,
        )
        .expect_err("public subprocess backend must not start");
        assert!(error.contains("unisolated subprocess"), "{error}");

        Config::from_sources(
            &source(&[
                ("COOP_ADDR", "0.0.0.0:7300"),
                ("COOP_UNSAFE_ALLOW_PUBLIC_DEV", "true"),
            ]),
            false,
        )
        .expect("conspicuous acknowledgement is explicit");
    }

    #[test]
    fn configured_resource_ceilings_are_coherent_and_clamp_memory() {
        let cfg = Config::from_sources(
            &source(&[
                ("COOP_MAX_JOB_MEM_MB", "512"),
                ("COOP_MEMORY_BUDGET_MB", "1024"),
                ("COOP_STORAGE_TENANT_MB", "128"),
                ("COOP_STORAGE_GLOBAL_MB", "256"),
            ]),
            false,
        )
        .unwrap();
        let limits = coop_types::Limits {
            mem_mb: 4096,
            ..coop_types::Limits::default()
        };
        assert_eq!(cfg.clamp_limits(limits).mem_mb, 512);

        let error = Config::from_sources(
            &source(&[
                ("COOP_MAX_JOB_MEM_MB", "2048"),
                ("COOP_MEMORY_BUDGET_MB", "1024"),
            ]),
            false,
        )
        .expect_err("one job cannot exceed aggregate memory");
        assert!(error.contains("must not exceed"), "{error}");
    }
}
