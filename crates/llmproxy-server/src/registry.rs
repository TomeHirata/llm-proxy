use std::{collections::HashMap, sync::Arc};

use llmproxy_core::{error::ProxyError, provider::Credential, provider::Provider};
use llmproxy_providers::{AnthropicProvider, BedrockProvider, GeminiProvider, PassthroughProvider};

use crate::config::{AppConfig, ProviderConfig};

pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    /// Credentials loaded from the config file, keyed by provider name.
    config_creds: HashMap<String, Credential>,
    /// Config-file region (used for Bedrock when no `AWS_REGION` env var).
    bedrock_region: Option<String>,
}

impl ProviderRegistry {
    pub fn from_config(cfg: &AppConfig) -> Self {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        let mut config_creds: HashMap<String, Credential> = HashMap::new();
        let mut bedrock_region: Option<String> = None;

        providers.insert("openai".into(), Arc::new(PassthroughProvider::openai()));
        providers.insert("mistral".into(), Arc::new(PassthroughProvider::mistral()));
        providers.insert(
            "togetherai".into(),
            Arc::new(PassthroughProvider::togetherai()),
        );
        providers.insert("anthropic".into(), Arc::new(AnthropicProvider::new()));
        providers.insert("gemini".into(), Arc::new(GeminiProvider::new()));
        providers.insert("bedrock".into(), Arc::new(BedrockProvider::new()));

        // Azure requires endpoint + api_version, so it's only registered when
        // both are present in the config.
        if let Some(p) = cfg.providers.get("azure") {
            if let (Some(endpoint), Some(api_version)) = (&p.endpoint, &p.api_version) {
                providers.insert(
                    "azure".into(),
                    Arc::new(PassthroughProvider::azure(
                        endpoint.clone(),
                        api_version.clone(),
                    )),
                );
            }
        }

        // Databricks requires a non-empty workspace URL; treat whitespace-only
        // (e.g. unset ${ENV_VAR} interpolation) the same as absent.
        if let Some(p) = cfg.providers.get("databricks") {
            if let Some(endpoint) = p
                .endpoint
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                providers.insert(
                    "databricks".into(),
                    Arc::new(PassthroughProvider::databricks(endpoint.to_string())),
                );
            }
        }

        for (name, p) in &cfg.providers {
            if let Some(cred) = provider_credential(name, p) {
                config_creds.insert(name.clone(), cred);
            }
            if name == "bedrock" {
                bedrock_region = p.region.clone();
            }
        }

        Self {
            providers,
            config_creds,
            bedrock_region,
        }
    }

    /// List registered providers alongside whether a credential resolves for
    /// them *right now* (config or well-known env var; for Bedrock: all of
    /// access key, secret key, region). This is what gets printed by
    /// `llmproxy providers`.
    pub fn configured_names(&self) -> Vec<(String, bool)> {
        let mut names: Vec<_> = self.providers.keys().cloned().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let usable = self.resolve_credential(&n, None).is_ok();
                (n, usable)
            })
            .collect()
    }

    pub fn provider_names(&self) -> Vec<String> {
        let mut v: Vec<_> = self.providers.keys().cloned().collect();
        v.sort();
        v
    }

    /// Parse `"provider/model_id"`, resolve the credential, and return the
    /// provider handle + native model id + credential.
    pub fn resolve(
        &self,
        model_field: &str,
        auth_header: Option<&str>,
    ) -> Result<(Arc<dyn Provider>, String, Credential), ProxyError> {
        let (provider_name, model_id) = model_field.split_once('/').ok_or_else(|| {
            ProxyError::ModelNotFound(format!(
                "model must be in 'provider/model_id' format, got: '{}'",
                model_field
            ))
        })?;

        let provider = self.providers.get(provider_name).ok_or_else(|| {
            ProxyError::ModelNotFound(format!("provider '{}' is not configured", provider_name))
        })?;

        let cred = self.resolve_credential(provider_name, auth_header)?;
        Ok((Arc::clone(provider), model_id.to_string(), cred))
    }

    pub fn credential_for(
        &self,
        provider_name: &str,
        auth_header: Option<&str>,
    ) -> Result<Credential, ProxyError> {
        self.resolve_credential(provider_name, auth_header)
    }

    fn resolve_credential(
        &self,
        provider_name: &str,
        auth_header: Option<&str>,
    ) -> Result<Credential, ProxyError> {
        // Bedrock is AWS-only — always constructed from env/config, never the
        // Authorization header.
        if provider_name == "bedrock" {
            return self.resolve_aws_credential();
        }

        if let Some(token) = auth_header.and_then(parse_bearer_token) {
            return Ok(Credential::BearerToken(token));
        }

        if let Some(cred) = self.config_creds.get(provider_name) {
            return Ok(cred.clone());
        }

        if let Some(env_key) = env_key_for(provider_name) {
            if let Ok(v) = std::env::var(env_key) {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Ok(Credential::BearerToken(trimmed.to_string()));
                }
            }
        }

        Err(ProxyError::Config(format!(
            "no credentials for provider '{}' (pass an Authorization header, \
             set the provider API key in config, or set the {} env var)",
            provider_name,
            env_key_for(provider_name).unwrap_or("corresponding"),
        )))
    }

    fn resolve_aws_credential(&self) -> Result<Credential, ProxyError> {
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID").map_err(|_| {
            ProxyError::Config(
                "AWS_ACCESS_KEY_ID env var is required for bedrock \
                 (shared-credentials / profile files are not supported in v0.1)"
                    .into(),
            )
        })?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY").map_err(|_| {
            ProxyError::Config(
                "AWS_SECRET_ACCESS_KEY env var is required for bedrock \
                 (shared-credentials / profile files are not supported in v0.1)"
                    .into(),
            )
        })?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
        let region = std::env::var("AWS_REGION")
            .ok()
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
            .or_else(|| self.bedrock_region.clone())
            .ok_or_else(|| {
                ProxyError::Config(
                    "no AWS region (set AWS_REGION env var or bedrock.region in config)".into(),
                )
            })?;

        Ok(Credential::AwsSigV4 {
            access_key_id,
            secret_access_key,
            session_token,
            region,
        })
    }
}

/// Extract a token from an `Authorization` header value. The scheme comparison
/// is case-insensitive per RFC 7235.
fn parse_bearer_token(header: &str) -> Option<String> {
    let trimmed = header.trim();
    let (scheme, rest) = trimmed.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn env_key_for(provider_name: &str) -> Option<&'static str> {
    match provider_name {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "cohere" => Some("COHERE_API_KEY"),
        "togetherai" => Some("TOGETHERAI_API_KEY"),
        "azure" => Some("AZURE_OPENAI_API_KEY"),
        "databricks" => Some("DATABRICKS_TOKEN"),
        _ => None,
    }
}

fn provider_credential(name: &str, p: &ProviderConfig) -> Option<Credential> {
    if name == "bedrock" {
        // Bedrock creds come from env, never from the YAML file.
        return None;
    }
    // After `${VAR}` interpolation a missing env var becomes an empty string;
    // treat that as "no credential" rather than forwarding an empty token.
    p.api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Credential::BearerToken(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ProviderConfig, ServerConfig};
    use serial_test::serial;

    /// RAII guard that snapshots env vars on construction and restores them on
    /// drop, so tests can't leak into each other or clobber a developer's
    /// real environment.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var(*k).ok())).collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    fn base_cfg() -> AppConfig {
        let mut cfg = AppConfig {
            server: ServerConfig::default(),
            providers: HashMap::new(),
            ..Default::default()
        };
        cfg.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                api_key: Some("from-config".into()),
                ..Default::default()
            },
        );
        cfg
    }

    #[test]
    fn resolve_uses_header_over_config() {
        let reg = ProviderRegistry::from_config(&base_cfg());
        let (_, id, cred) = reg
            .resolve("anthropic/claude", Some("Bearer sk-from-header"))
            .unwrap();
        assert_eq!(id, "claude");
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "sk-from-header"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let (_, _, cred) = reg.resolve("anthropic/x", Some("bearer sk-lower")).unwrap();
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "sk-lower"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn non_bearer_scheme_is_ignored() {
        // A non-Bearer scheme should fall through to config/env resolution,
        // not be used as a token.
        let reg = ProviderRegistry::from_config(&base_cfg());
        let (_, _, cred) = reg
            .resolve("anthropic/x", Some("Basic dXNlcjpwYXNz"))
            .unwrap();
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "from-config"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn resolve_falls_back_to_config() {
        let reg = ProviderRegistry::from_config(&base_cfg());
        let (_, _, cred) = reg.resolve("anthropic/claude", None).unwrap();
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "from-config"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn empty_config_api_key_falls_through() {
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                api_key: Some("   ".into()),
                ..Default::default()
            },
        );
        let reg = ProviderRegistry::from_config(&cfg);
        // No env var and no header: should error, not forward an empty token.
        let err = reg
            .resolve("anthropic/claude", None)
            .err()
            .expect("expected error");
        assert!(matches!(err, ProxyError::Config(_)));
    }

    #[test]
    #[serial(env)]
    fn resolve_falls_back_to_env_var() {
        let _g = EnvGuard::new(&["OPENAI_API_KEY"]);
        std::env::set_var("OPENAI_API_KEY", "from-env");
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let (_, _, cred) = reg.resolve("openai/gpt-4o", None).unwrap();
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "from-env"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    #[serial(env)]
    fn databricks_token_env_var_fallback() {
        let _g = EnvGuard::new(&["DATABRICKS_TOKEN"]);
        std::env::set_var("DATABRICKS_TOKEN", "dapi-from-env");
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "databricks".into(),
            ProviderConfig {
                endpoint: Some("https://my-workspace.azuredatabricks.net".into()),
                ..Default::default()
            },
        );
        let reg = ProviderRegistry::from_config(&cfg);
        let (_, id, cred) = reg
            .resolve("databricks/databricks-mixtral-8x7b", None)
            .unwrap();
        assert_eq!(id, "databricks-mixtral-8x7b");
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "dapi-from-env"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn databricks_empty_endpoint_not_registered() {
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "databricks".into(),
            ProviderConfig {
                endpoint: Some("   ".into()),
                ..Default::default()
            },
        );
        let reg = ProviderRegistry::from_config(&cfg);
        let err = reg
            .resolve("databricks/any-model", None)
            .err()
            .expect("expected error");
        assert!(matches!(err, ProxyError::ModelNotFound(_)));
    }

    #[test]
    fn resolve_bad_format_errors() {
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let err = reg
            .resolve("badformat", None)
            .err()
            .expect("expected error");
        assert!(matches!(err, ProxyError::ModelNotFound(_)));
    }

    #[test]
    fn resolve_unknown_provider_errors() {
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let err = reg
            .resolve("unknown/model", None)
            .err()
            .expect("expected error");
        assert!(matches!(err, ProxyError::ModelNotFound(_)));
    }

    #[test]
    #[serial(env)]
    fn bedrock_ignores_bearer_header() {
        let _g = EnvGuard::new(&["AWS_ACCESS_KEY_ID"]);
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let err = reg
            .resolve("bedrock/amazon.nova-pro-v1:0", Some("Bearer ignore"))
            .err()
            .expect("expected error");
        assert!(matches!(err, ProxyError::Config(_)));
    }

    #[test]
    #[serial(env)]
    fn model_id_preserves_slashes() {
        let _g = EnvGuard::new(&["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_REGION"]);
        std::env::set_var("AWS_ACCESS_KEY_ID", "x");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "y");
        std::env::set_var("AWS_REGION", "us-east-1");
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let (_, id, _) = reg
            .resolve("bedrock/us.anthropic.claude-3-5-sonnet-20241022-v2:0", None)
            .unwrap();
        assert_eq!(id, "us.anthropic.claude-3-5-sonnet-20241022-v2:0");
    }

    #[test]
    #[serial(env)]
    fn configured_names_requires_usable_cred() {
        // Azure registered without api_key shouldn't be flagged usable.
        let _g = EnvGuard::new(&[
            "AZURE_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "MISTRAL_API_KEY",
            "TOGETHERAI_API_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
        ]);
        for k in [
            "AZURE_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "MISTRAL_API_KEY",
            "TOGETHERAI_API_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
        ] {
            std::env::remove_var(k);
        }

        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "azure".into(),
            ProviderConfig {
                api_key: None,
                endpoint: Some("https://x".into()),
                api_version: Some("2024-02-01".into()),
                region: None,
            },
        );
        let reg = ProviderRegistry::from_config(&cfg);
        let names: HashMap<_, _> = reg.configured_names().into_iter().collect();
        assert_eq!(names.get("azure"), Some(&false));
        assert_eq!(names.get("openai"), Some(&false));
        assert_eq!(names.get("bedrock"), Some(&false));
    }

    #[test]
    fn parse_bearer_rejects_empty_and_other_schemes() {
        assert_eq!(parse_bearer_token("Bearer sk-1"), Some("sk-1".into()));
        assert_eq!(parse_bearer_token("bearer sk-2"), Some("sk-2".into()));
        assert_eq!(parse_bearer_token("BEARER sk-3"), Some("sk-3".into()));
        assert_eq!(parse_bearer_token("Bearer  sk-4 "), Some("sk-4".into()));
        assert_eq!(parse_bearer_token("Bearer "), None);
        assert_eq!(parse_bearer_token("Basic abc"), None);
        assert_eq!(parse_bearer_token(""), None);
    }
}
