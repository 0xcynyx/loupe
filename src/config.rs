//! Runtime settings resolved from process env plus an optional ENV_FILE.

use std::collections::HashMap;
use std::fs;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub gui_port: u16,
    pub bind: String,
    pub row_cap: usize,
    pub sort_cap: usize,
}

impl Config {
    // Process env wins over the env file, both win over defaults.
    pub fn load() -> Self {
        let file = read_env_file(&std::env::var("ENV_FILE").unwrap_or_default());
        let get = |key: &str, fallback: &str| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| file.get(key).cloned().filter(|v| !v.is_empty()))
                .unwrap_or_else(|| fallback.to_string())
        };
        Self {
            host: get("MILVUS_HOST", ""),
            port: get("MILVUS_PORT", "19530"),
            user: get("MILVUS_USER", "root"),
            password: get("MILVUS_PASSWORD", ""),
            gui_port: get("MILVUS_GUI_PORT", "3003").parse().unwrap_or(3003),
            bind: get("MILVUS_GUI_BIND", "127.0.0.1"),
            row_cap: get("MILVUS_GUI_ROW_CAP", "200").parse().unwrap_or(200),
            sort_cap: get("MILVUS_GUI_SORT_CAP", "500000").parse().unwrap_or(500_000),
        }
    }
}

// Parses simple KEY=VALUE lines, ignoring comments and blanks.
fn read_env_file(path: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    if path.is_empty() {
        return values;
    }
    let Ok(body) = fs::read_to_string(path) else {
        return values;
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let cleaned = value.trim().trim_matches('"').trim_matches('\'');
            values.insert(key.trim().to_string(), cleaned.to_string());
        }
    }
    values
}
