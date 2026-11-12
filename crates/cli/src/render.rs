//! Renderer linear (append-only ke scrollback).
//!
//! Teks model ke stdout (agar pipe/redirect bersih); jejak tool & ringkasan
//! ke stderr. Semua teks tak tepercaya dibersihkan dulu lewat `text::sanitize`
//! agar escape sequence dari model/tool tidak bisa mengacaukan terminal.

pub mod json;
pub mod text;

use crate::render::text::{format_duration, preview_line, sanitize, summarize_tool};
use minic_core::agent::RunOutcome;
use minic_core::events::{AgentEvent, DecisionKind};
use std::io::Write;

pub fn handle(event: &AgentEvent, verbose: bool) {
    match event {
        AgentEvent::TextDelta { text } => {
            print!("{}", sanitize(text));
            let _ = std::io::stdout().flush();
        }
        AgentEvent::ReasoningDelta { text } => {
            if verbose {
                eprint!("· {}", sanitize(text));
                let _ = std::io::stderr().flush();
            }
        }
        AgentEvent::ToolCallStarted {
            name, arguments, ..
        } => {
            let summary = summarize_tool(name, arguments);
            if summary.is_empty() {
                eprintln!("\n  › {name}");
            } else {
                eprintln!("\n  › {name}  {summary}");
            }
            let _ = std::io::stderr().flush();
        }
        AgentEvent::ToolCallFinished {
            is_error,
            duration_ms,
            result_preview,
            ..
        } => {
            let mark = if *is_error { "✗" } else { "✓" };
            eprintln!("    {mark} {}", format_duration(*duration_ms));
            if *is_error {
                let preview = preview_line(result_preview);
                if !preview.is_empty() {
                    eprintln!("      {preview}");
                }
            }
        }
        AgentEvent::Error { message } => {
            eprintln!("! {message}");
        }
        AgentEvent::ContextCompacted { elided } => {
            eprintln!("· compacted {elided} message(s)");
        }
        AgentEvent::ToolDecided {
            name,
            decision,
            reason,
            ..
        } => {
            // Hanya keputusan menolak yang perlu terlihat di scrollback;
            // allow adalah keadaan normal dan akan berisik.
            if *decision == DecisionKind::Deny {
                eprintln!("    ⊘ {name} denied · {reason}");
                let _ = std::io::stderr().flush();
            }
        }
        AgentEvent::Note { text } => {
            eprintln!("    · {text}");
            let _ = std::io::stderr().flush();
        }
        AgentEvent::TurnStarted { .. }
        | AgentEvent::TurnFinished { .. }
        | AgentEvent::AgentFinished { .. } => {}
    }
}

/// Ringkasan satu baris setelah turn selesai (stderr, agar stdout tetap teks).
pub fn final_summary(outcome: &RunOutcome) {
    if outcome.steps > 0 {
        eprintln!(
            "· {} step(s) · {} tool call(s)",
            outcome.steps, outcome.tool_calls
        );
    }
}
