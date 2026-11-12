//! Tipe domain agen. Bebas I/O — hanya data + helper konstruksi.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text { text: String },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text { text: text.into() }
    }
}

/// Panggilan tool dari model (id, nama, argumen JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Hasil eksekusi tool yang dikembalikan ke model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn err(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Jejak reasoning/thinking model (dibawa lintas resume; lihat pelajaran F-05 baseline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub is_error: bool,
}

impl Message {
    fn base(role: Role) -> Self {
        Self {
            role,
            content: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning: None,
            is_error: false,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        let mut m = Self::base(Role::System);
        m.content.push(Content::text(text));
        m
    }

    pub fn user(text: impl Into<String>) -> Self {
        let mut m = Self::base(Role::User);
        m.content.push(Content::text(text));
        m
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        let mut m = Self::base(Role::Assistant);
        m.content.push(Content::text(text));
        m
    }

    /// Pesan assistant yang memuat teks (opsional) + tool calls.
    pub fn assistant_with(text: impl Into<String>, calls: Vec<ToolCall>) -> Self {
        let mut m = Self::base(Role::Assistant);
        let text = text.into();
        if !text.is_empty() {
            m.content.push(Content::text(text));
        }
        m.tool_calls = calls;
        m
    }

    pub fn tool_result(result: ToolResult) -> Self {
        let mut m = Self::base(Role::Tool);
        m.content.push(Content::text(result.content));
        m.tool_call_id = Some(result.tool_call_id);
        m.name = Some(result.name);
        m.is_error = result.is_error;
        m
    }

    /// Gabungan semua bagian teks (dipakai untuk wire + estimasi token).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            match part {
                Content::Text { text } => out.push_str(text),
            }
        }
        out
    }
}

/// Id sesi kanonik (newtype: tak bisa tertukar dengan id lain).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
