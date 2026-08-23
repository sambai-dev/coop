use std::collections::HashMap;

pub const DEV_DEFAULT_API_KEY: &str = "local:coop-dev-key";

#[derive(Clone)]
pub struct Config {
    pub addr: String,
    pub db_path: String,
    pub api_keys: HashMap<String, String>,
    pub workers: usize,
    pub tenant_concurrency: usize,
    pub rate_per_min: u32,
    pub sandbox: String,
    pub jobs_root: String,
    pub python_bin: Option<String>,
    pub node_bin: Option<String>,
    pub bash_bin: Option<String>,
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
            .field("workers", &self.workers)
            .field("tenant_concurrency", &self.tenant_concurrency)
            .field("rate_per_min", &self.rate_per_min)
            .field("sandbox", &self.sandbox)
            .field("jobs_root", &self.jobs_root)
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
        let mut api_keys = HashMap::new();
        let raw = getenv("COOP_API_KEYS").filter(|v| !v.trim().is_empty());
        let raw = match raw {
            Some(raw) => raw,
            None if production => {
                return Err(
                    "COOP_API_KEYS must be configured in production; refusing to start with the \
                     development default API key"
                        .to_string(),
                );
            }
            None => {
                tracing::warn!(
                    "SECURITY: no COOP_API_KEYS configured — falling back to the PUBLIC development \
                     default key '{DEV_DEFAULT_API_KEY}'. Anyone who can reach this server can run \
                     code on it. Set COOP_API_KEYS before exposing coop beyond localhost."
                );
                DEV_DEFAULT_API_KEY.to_string()
            }
        };
        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            match entry.split_once(':') {
                Some((tenant, key)) => {
                    api_keys.insert(key.to_string(), tenant.to_string());
                }
                None => {
                    api_keys.insert(entry.to_string(), "local".to_string());
                }
            }
        }

        Ok(Self {
            addr: env_or(getenv, "COOP_ADDR", "127.0.0.1:7300"),
            db_path: env_or(getenv, "COOP_DB", "coop.db"),
            api_keys,
            workers: env_or(getenv, "COOP_WORKERS", "4")
                .parse()
                .unwrap_or(4)
                .max(1),
            tenant_concurrency: env_or(getenv, "COOP_TENANT_CONCURRENCY", "2")
                .parse()
                .unwrap_or(2)
                .max(1),
            rate_per_min: env_or(getenv, "COOP_RATE_PER_MIN", "120")
                .parse()
                .unwrap_or(120),
            sandbox: env_or(getenv, "COOP_SANDBOX", "auto"),
            jobs_root: env_or(getenv, "COOP_JOBS_ROOT", &default_jobs_root()),
            python_bin: getenv("COOP_PYTHON"),
            node_bin: getenv("COOP_NODE"),
            bash_bin: getenv("COOP_BASH"),
        })
    }

    pub fn interpreter_override(&self, language: &str) -> Option<String> {
        match language {
            "python" => self.python_bin.clone(),
            "node" => self.node_bin.clone(),
            _ => self.bash_bin.clone(),
        }
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
        let cfg = Config::from_sources(&source(&[("COOP_API_KEYS", "acme:s3cr3t")]), true).unwrap();
        assert_eq!(cfg.api_keys.get("s3cr3t").map(String::as_str), Some("acme"));
        assert!(!cfg.api_keys.contains_key("coop-dev-key"));
    }

    #[test]
    fn debug_output_never_contains_plaintext_keys() {
        let cfg =
            Config::from_sources(&source(&[("COOP_API_KEYS", "acme:s3cr3t-value")]), true).unwrap();
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("s3cr3t-value"), "leaked: {rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
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
}
