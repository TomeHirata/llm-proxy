use std::{collections::HashMap, path::PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8080
}

/// Credentials + settings for a single provider. Only fields meaningful for
/// each provider are read — unknown fields are ignored silently.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    /// Azure endpoint, e.g. `https://resource.openai.azure.com`.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Azure API version, e.g. `2024-02-01`.
    #[serde(default)]
    pub api_version: Option<String>,
    /// Bedrock region, e.g. `us-east-1`.
    #[serde(default)]
    pub region: Option<String>,
}

/// Load config from the first file that exists, with `${ENV_VAR}` interpolation.
///
/// Priority (highest first):
/// 1. `--config <path>` argument
/// 2. `$LLMPROXY_CONFIG` env var
/// 3. `~/.config/llmproxy/config.yaml`
/// 4. `./llmproxy.yaml`
///
/// Returns the default (empty) config if no file is found — the proxy works
/// with zero config when clients pass API keys via the `Authorization` header.
pub fn load_config(cli_path: Option<&str>) -> anyhow::Result<AppConfig> {
    let paths: Vec<PathBuf> = {
        let mut p = Vec::new();
        if let Some(c) = cli_path {
            p.push(PathBuf::from(c));
        }
        if let Ok(env) = std::env::var("LLMPROXY_CONFIG") {
            p.push(PathBuf::from(env));
        }
        if let Some(home) = dirs::home_dir() {
            p.push(home.join(".config/llmproxy/config.yaml"));
        }
        p.push(PathBuf::from("llmproxy.yaml"));
        p
    };

    for path in &paths {
        if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            let interpolated = interpolate_env(&raw);
            let cfg: AppConfig = serde_yaml::from_str(&interpolated)?;
            return Ok(cfg);
        }
    }

    Ok(AppConfig::default())
}

pub fn interpolate_env(s: &str) -> String {
    let re = Regex::new(r"\$\{([^}]+)\}").unwrap();
    re.replace_all(s, |caps: &regex::Captures| {
        std::env::var(&caps[1]).unwrap_or_default()
    })
    .into_owned()
}

/// Returns a copy of the config with all secret values redacted to `***`.
pub fn redacted(cfg: &AppConfig) -> AppConfig {
    let mut out = cfg.clone();
    for p in out.providers.values_mut() {
        if p.api_key.is_some() {
            p.api_key = Some("***".into());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_env_vars() {
        std::env::set_var("LLMPROXY_TEST_KEY", "abc123");
        let interp = interpolate_env("api_key: ${LLMPROXY_TEST_KEY}");
        assert_eq!(interp, "api_key: abc123");
    }

    #[test]
    fn missing_env_becomes_empty() {
        std::env::remove_var("LLMPROXY_MISSING_XYZ");
        let interp = interpolate_env("api_key: ${LLMPROXY_MISSING_XYZ}");
        assert_eq!(interp, "api_key: ");
    }

    #[test]
    fn redact_replaces_keys() {
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "openai".into(),
            ProviderConfig {
                api_key: Some("sk-real".into()),
                ..Default::default()
            },
        );
        let r = redacted(&cfg);
        assert_eq!(r.providers["openai"].api_key.as_deref(), Some("***"));
    }
}
