//! Taksonomi error agen. Satu `enum` kanonik — bukan string bebas.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("timeout")]
    Timeout,

    #[error("network error: {0}")]
    Network(String),

    #[error("rate limited: {0}")]
    RateLimit(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("permission denied: {0}")]
    Permission(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("aborted")]
    Aborted,

    #[error("provider error: {0}")]
    Provider(String),

    /// Batas langkah agen tercapai (bukan error provider).
    #[error("max steps exceeded: {0}")]
    MaxSteps(String),
}

impl AgentError {
    /// Apakah kegagalan ini layak dicoba ulang (retry) oleh host?
    pub fn is_retryable(&self) -> bool {
        matches!(self, AgentError::Network(_) | AgentError::RateLimit(_))
    }
}
