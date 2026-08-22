use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: String,
    pub db_path: String,
    pub api_keys: HashMap<String, String>,
    pub workers: usize,
    pub tenant_concurrency: usize,
    pub rate_per_min: u32,
    pub sandbox: String,
    pub python_bin: Option<String>,
    pub node_bin: Option<String>,
    pub bash_bin: Option<String>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Self {
        let mut api_keys = HashMap::new();
        let raw = env_or("COOP_API_KEYS", "local:coop-dev-key");
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

        Self {
            addr: env_or("COOP_ADDR", "127.0.0.1:7300"),
            db_path: env_or("COOP_DB", "coop.db"),
            api_keys,
            workers: env_or("COOP_WORKERS", "4").parse().unwrap_or(4).max(1),
            tenant_concurrency: env_or("COOP_TENANT_CONCURRENCY", "2")
                .parse()
                .unwrap_or(2)
                .max(1),
            rate_per_min: env_or("COOP_RATE_PER_MIN", "120").parse().unwrap_or(120),
            sandbox: env_or("COOP_SANDBOX", "auto"),
            python_bin: std::env::var("COOP_PYTHON").ok(),
            node_bin: std::env::var("COOP_NODE").ok(),
            bash_bin: std::env::var("COOP_BASH").ok(),
        }
    }

    pub fn interpreter_override(&self, language: &str) -> Option<String> {
        match language {
            "python" => self.python_bin.clone(),
            "node" => self.node_bin.clone(),
            _ => self.bash_bin.clone(),
        }
    }
}
