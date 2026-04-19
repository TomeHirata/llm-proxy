use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("AWS error: {0}")]
    Aws(String),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Provider config error: {0}")]
    Config(String),

    #[error("Upstream error {status}: {body}")]
    Upstream { status: u16, body: String },

    #[error("Stream error: {0}")]
    Stream(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),
}
