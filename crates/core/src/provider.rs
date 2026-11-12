//! Abstraksi provider model. Host menyediakan implementasi (Anthropic,
//! OpenAI-compatible, Responses, atau fake). Kernel hanya melihat stream event.

use crate::error::AgentError;
use crate::types::Message;
use futures_core::Stream;
use std::pin::Pin;

/// Permintaan satu putaran ke model.
#[derive(Debug, Clone, Default)]
pub struct StreamRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    /// Skema tool dalam format JSON (bentuk wire, mis. OpenAI `tools`).
    pub tools: Vec<serde_json::Value>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    #[default]
    Unknown,
}

/// Event dari provider. Argumen tool datang bertahap, host mengakumulasi.
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args_fragment: String,
    },
    Finish(FinishReason),
}

/// Stream event yang sudah di-type-erase (memakai `futures_core::Stream`).
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

pub trait ModelProvider: Send + Sync {
    /// Mulai satu putaran streaming. Stream harus `'static` (meng-own datanya).
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>>;
}
