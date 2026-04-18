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

        // Always register the six MVP providers — credential resolution happens
        // per-request, so "configured" here means the provider code is available.
        // A provider is only usable if a credential resolves at request time.
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
        // configured.
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

    pub fn configured_names(&self, cfg: &AppConfig) -> Vec<(String, bool)> {
        let mut names: Vec<_> = self.providers.keys().cloned().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let has_cred = self.config_creds.contains_key(&n)
                    || has_env_cred(&n)
                    || cfg.providers.contains_key(&n);
                (n, has_cred)
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

        if let Some(h) = auth_header {
            let token = h.trim_start_matches("Bearer ").trim().to_string();
            if !token.is_empty() {
                return Ok(Credential::BearerToken(token));
            }
        }

        if let Some(cred) = self.config_creds.get(provider_name) {
            return Ok(cred.clone());
        }

        if let Some(env_key) = env_key_for(provider_name) {
            if let Ok(v) = std::env::var(env_key) {
                if !v.is_empty() {
                    return Ok(Credential::BearerToken(v));
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
            ProxyError::Config("AWS_ACCESS_KEY_ID env var is required for bedrock".into())
        })?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY").map_err(|_| {
            ProxyError::Config("AWS_SECRET_ACCESS_KEY env var is required for bedrock".into())
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

fn env_key_for(provider_name: &str) -> Option<&'static str> {
    match provider_name {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "cohere" => Some("COHERE_API_KEY"),
        "togetherai" => Some("TOGETHERAI_API_KEY"),
        "azure" => Some("AZURE_OPENAI_API_KEY"),
        _ => None,
    }
}

fn has_env_cred(provider_name: &str) -> bool {
    if provider_name == "bedrock" {
        return std::env::var("AWS_ACCESS_KEY_ID").is_ok();
    }
    env_key_for(provider_name)
        .map(|k| std::env::var(k).is_ok())
        .unwrap_or(false)
}

fn provider_credential(name: &str, p: &ProviderConfig) -> Option<Credential> {
    if name == "bedrock" {
        // Bedrock creds come from env, never from the YAML file.
        return None;
    }
    p.api_key.clone().map(Credential::BearerToken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ProviderConfig, ServerConfig};
    use serial_test::serial;

    fn base_cfg() -> AppConfig {
        let mut cfg = AppConfig {
            server: ServerConfig::default(),
            providers: HashMap::new(),
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
    fn resolve_falls_back_to_config() {
        let reg = ProviderRegistry::from_config(&base_cfg());
        let (_, _, cred) = reg.resolve("anthropic/claude", None).unwrap();
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "from-config"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    #[serial(env)]
    fn resolve_falls_back_to_env_var() {
        std::env::set_var("OPENAI_API_KEY", "from-env");
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let (_, _, cred) = reg.resolve("openai/gpt-4o", None).unwrap();
        match cred {
            Credential::BearerToken(s) => assert_eq!(s, "from-env"),
            _ => panic!("wrong variant"),
        }
        std::env::remove_var("OPENAI_API_KEY");
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
        std::env::set_var("AWS_ACCESS_KEY_ID", "x");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "y");
        std::env::set_var("AWS_REGION", "us-east-1");
        let reg = ProviderRegistry::from_config(&AppConfig::default());
        let (_, id, _) = reg
            .resolve("bedrock/us.anthropic.claude-3-5-sonnet-20241022-v2:0", None)
            .unwrap();
        assert_eq!(id, "us.anthropic.claude-3-5-sonnet-20241022-v2:0");
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::remove_var("AWS_REGION");
    }
}
