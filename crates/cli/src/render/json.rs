//! Renderer JSONL: satu event agen per baris, untuk konsumsi mesin dan
//! conformance. Bentuk kanonik: `{"v":1,"event":"...", ...}`.
//!
//! Dijamin deterministik: nilai dibangun lewat `serde_json::json!` dan
//! dibandingkan berdasarkan nilai (bukan urutan kunci).

use minic_core::events::AgentEvent;
use serde_json::{Value, json};

pub const SCHEMA_VERSION: u32 = 1;

pub fn event_value(event: &AgentEvent) -> Value {
    match event {
        AgentEvent::TurnStarted { turn } => {
            json!({ "v": SCHEMA_VERSION, "event": "turn_started", "turn": turn })
        }
        AgentEvent::TextDelta { text } => {
            json!({ "v": SCHEMA_VERSION, "event": "text_delta", "text": text })
        }
        AgentEvent::ReasoningDelta { text } => {
            json!({ "v": SCHEMA_VERSION, "event": "reasoning_delta", "text": text })
        }
        AgentEvent::ToolCallStarted { id, name, .. } => {
            json!({ "v": SCHEMA_VERSION, "event": "tool_call_started", "id": id, "name": name })
        }
        AgentEvent::ToolCallFinished {
            id, name, is_error, ..
        } => {
            json!({
                "v": SCHEMA_VERSION,
                "event": "tool_call_finished",
                "id": id,
                "name": name,
                "is_error": is_error
            })
        }
        AgentEvent::TurnFinished { turn } => {
            json!({ "v": SCHEMA_VERSION, "event": "turn_finished", "turn": turn })
        }
        AgentEvent::AgentFinished { steps } => {
            json!({ "v": SCHEMA_VERSION, "event": "agent_finished", "steps": steps })
        }
        AgentEvent::ContextCompacted { elided } => {
            json!({ "v": SCHEMA_VERSION, "event": "context_compacted", "elided": elided })
        }
        AgentEvent::ToolDecided {
            id,
            name,
            decision,
            reason,
        } => {
            json!({
                "v": SCHEMA_VERSION,
                "event": "tool_decided",
                "id": id,
                "name": name,
                "decision": decision.as_str(),
                "reason": reason
            })
        }
        AgentEvent::Error { message } => {
            json!({ "v": SCHEMA_VERSION, "event": "error", "message": message })
        }
        AgentEvent::Note { text } => {
            json!({ "v": SCHEMA_VERSION, "event": "note", "text": text })
        }
    }
}

pub fn line(event: &AgentEvent) -> String {
    event_value(event).to_string()
}

pub fn write(event: &AgentEvent) {
    println!("{}", line(event));
}

/// Baris pembuka yang menandai sesi.
pub fn session_line(session_id: &str, model: &str) -> String {
    json!({
        "v": SCHEMA_VERSION,
        "event": "session",
        "session_id": session_id,
        "model": model
    })
    .to_string()
}
