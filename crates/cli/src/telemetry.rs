//! Telemetry lokal (opt-in).
//!
//! minic **tidak mengirim telemetry apa pun** ke jaringan. Yang ada hanyalah
//! jejak lokal (JSONL) untuk debugging, dan itu pun **opt-in**:
//!
//! - `MINIC_TELEMETRY` tidak di-set / `0` → nonaktif.
//! - `MINIC_TELEMETRY=1` → tulis `.minic/trace.jsonl` (project-local).
//!
//! Kill-switch `MINIC_TELEMETRY=0` selalu menang. Mengabaikan kegagalan I/O
//! (telemetry tidak boleh menggagalkan turn) dan tidak menyimpan isi prompt.

use minic_core::events::AgentEvent;
use std::path::PathBuf;

pub struct Telemetry {
    path: Option<PathBuf>,
    enabled: bool,
}

impl Telemetry {
    pub fn from_env(root: &std::path::Path) -> Self {
        let enabled = std::env::var("MINIC_TELEMETRY")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let path = enabled.then(|| crate::config::minic_dir(root).join("trace.jsonl"));
        Self { path, enabled }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Tulis satu event lokal. Kegagalan I/O diabaikan.
    pub fn event(&self, kind: &str, detail: serde_json::Value) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = serde_json::json!({ "kind": kind, "detail": detail });
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Rekam event agen (hanya metadata; tanpa payload teks).
    pub fn record(&self, event: &AgentEvent) {
        if !self.enabled {
            return;
        }
        match event {
            AgentEvent::TurnStarted { turn } => {
                self.event("turn_started", serde_json::json!({ "turn": turn }));
            }
            AgentEvent::ToolCallStarted { name, .. } => {
                self.event("tool_started", serde_json::json!({ "name": name }));
            }
            AgentEvent::ToolCallFinished { name, is_error, .. } => {
                self.event(
                    "tool_finished",
                    serde_json::json!({ "name": name, "is_error": is_error }),
                );
            }
            AgentEvent::AgentFinished { steps } => {
                self.event("agent_finished", serde_json::json!({ "steps": steps }));
            }
            AgentEvent::ContextCompacted { elided } => {
                self.event("context_compacted", serde_json::json!({ "elided": elided }));
            }
            AgentEvent::Error { message } => {
                self.event("error", serde_json::json!({ "message": message }));
            }
            AgentEvent::Note { text } => {
                self.event("note", serde_json::json!({ "text": text }));
            }
            AgentEvent::ToolDecided {
                name,
                decision,
                reason,
                ..
            } => {
                // Tanpa argumen mentah: `reason` sudah dirangkum oleh policy
                // dan bisa memuat potongan perintah; host sengaja tidak pernah
                // mencatat isi berkas/secret di sini.
                self.event(
                    "tool_decision",
                    serde_json::json!({
                        "name": name,
                        "decision": decision.as_str(),
                        "reason": reason
                    }),
                );
            }
            AgentEvent::TextDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::TurnFinished { .. } => {}
        }
    }
}
