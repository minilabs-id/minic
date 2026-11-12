//! Driver loop agen (STATE/MODEL/ACTION/LOOP).
//!
//! Satu `run_agent` menjalankan siklus: kirim history ke model → terima teks +
//! panggilan tool → minta izin → eksekusi tool → tambahkan hasil → ulangi,
//! sampai model berhenti memanggil tool atau batas langkah tercapai.

use crate::compaction::{DEFAULT_KEEP_RECENT, budget_tokens, mechanical_compact};
use crate::error::AgentError;
use crate::events::{AgentEvent, DecisionKind};
use crate::history::ContextStore;
use crate::permission::{Decision, PermissionHandler};
use crate::provider::{ModelProvider, ProviderEvent, StreamRequest};
use crate::tokens::estimate_messages;
use crate::tool::{ToolContext, ToolRegistry};
use crate::types::{Message, Role, ToolCall, ToolResult};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_call_id() -> String {
    format!("call_{}", CALL_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Cuplikan satu baris untuk UI (bukan untuk model): buang baris kosong,
/// potong pada 160 karakter agar status/tool line tetap ringkas.
fn preview(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut chars = line.chars();
    let mut out: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// Hasil penuh (longgar) untuk panel detail di UI. Dipotong pada 16k karakter
/// agar satu perintah yang membanjiri output tidak menghabiskan memori UI;
/// kernel tetap tidak tahu apa-apa soal tampilan.
const DETAIL_CAP_CHARS: usize = 16_000;

fn detail(text: &str) -> String {
    let cleaned = text.trim_end();
    if cleaned.chars().count() <= DETAIL_CAP_CHARS {
        return cleaned.to_string();
    }
    let mut out: String = cleaned.chars().take(DETAIL_CAP_CHARS).collect();
    out.push_str("\n… [detail truncated]");
    out
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub model: String,
    pub system: Option<String>,
    pub max_steps: u32,
    pub max_tokens: Option<u32>,
    /// Context window model; `None` mematikan kompaksi.
    pub context_window_tokens: Option<u32>,
    /// Jumlah pesan terbaru yang selalu dipertahankan saat kompaksi.
    pub keep_recent: usize,
}

impl RunOptions {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            max_steps: 50,
            max_tokens: None,
            context_window_tokens: Some(128_000),
            keep_recent: DEFAULT_KEEP_RECENT,
        }
    }
}

/// Hook tool: diimplementasikan host. Default no-op.
pub trait AgentHooks: Send + Sync {
    /// Kembalikan `Err` untuk menolak tool sebelum dijalankan.
    fn pre_tool(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _cwd: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }

    fn post_tool(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _cwd: &std::path::Path,
        _exit_code: i32,
    ) -> Option<String> {
        None
    }
}

/// Persistensi history: dipanggil sinkron oleh kernel setiap kali history
/// berubah (akhir turn). Implementasi host (SQLite) — kernel tetap bebas I/O,
/// hanya memanggil trait.
pub trait HistoryPersistence: Send + Sync {
    /// Simpan snapshot history; gagal tidak menggagalkan agent (best-effort,
    /// agar kegagalan disk tidak menghancurkan turn yang sedang berjalan).
    fn save(&self, messages: &[Message]);
}

/// Hook no-op (default).
#[derive(Default)]
pub struct NoopHooks;

impl AgentHooks for NoopHooks {}

#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    pub steps: u32,
    pub text: String,
    pub tool_calls: u32,
}

/// Akumulator satu tool call selama streaming (argumen datang bertahap).
#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    args: String,
}

impl PendingToolCall {
    fn into_tool_call(self) -> Option<ToolCall> {
        let name = self.name?;
        let id = self.id.unwrap_or_else(next_call_id);
        let arguments = if self.args.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&self.args).unwrap_or(serde_json::Value::Null)
        };
        Some(ToolCall {
            id,
            name,
            arguments,
        })
    }
}

/// Jalankan agen sampai selesai. `on_event` dipanggil sinkron setiap event.
/// `persistence` (opsional): tiap akhir turn, snapshot history disimpan —
/// gagal tidak menggagalkan turn (best-effort, host yang menangani I/O).
// Parameter terikat kontrak pemanggil (composition root): memecahnya ke
// struct tidak menambah kejelasan, jadi beri pengecualian clippy.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent(
    provider: &dyn ModelProvider,
    tools: &ToolRegistry,
    permissions: &dyn PermissionHandler,
    hooks: &dyn AgentHooks,
    history: &mut ContextStore,
    tool_ctx: &ToolContext,
    options: &RunOptions,
    persistence: Option<&dyn HistoryPersistence>,
    on_event: &mut (dyn FnMut(AgentEvent) + Send),
) -> Result<RunOutcome, AgentError> {
    let mut outcome = RunOutcome::default();
    let tool_schemas = tools.schemas();

    for step in 0..options.max_steps {
        let turn = step + 1;

        // Kompaksi bila history mendekati budget konteks (lossy, mekanis).
        if let Some(window) = options.context_window_tokens {
            let limit = budget_tokens(window);
            if estimate_messages(history.messages()) > limit {
                let (compacted, elided) =
                    mechanical_compact(history.messages(), options.keep_recent);
                if elided > 0 {
                    history.set_messages(compacted);
                    on_event(AgentEvent::ContextCompacted { elided });
                }
            }
        }

        on_event(AgentEvent::TurnStarted { turn });

        let request = StreamRequest {
            model: options.model.clone(),
            system: options.system.clone(),
            messages: history.messages().to_vec(),
            tools: tool_schemas.clone(),
            max_tokens: options.max_tokens,
        };

        let mut stream = provider.stream(request);
        let mut text = String::new();
        let mut pending: Vec<PendingToolCall> = Vec::new();

        while let Some(item) = stream.next().await {
            match item? {
                ProviderEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    on_event(AgentEvent::TextDelta { text: delta });
                }
                ProviderEvent::ReasoningDelta(delta) => {
                    on_event(AgentEvent::ReasoningDelta { text: delta });
                }
                ProviderEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    args_fragment,
                } => {
                    while pending.len() <= index {
                        pending.push(PendingToolCall::default());
                    }
                    let slot = &mut pending[index];
                    if id.is_some() {
                        slot.id = id;
                    }
                    if name.is_some() {
                        slot.name = name;
                    }
                    slot.args.push_str(&args_fragment);
                }
                ProviderEvent::Finish(_) => {}
            }
        }

        let calls: Vec<ToolCall> = pending
            .into_iter()
            .filter_map(PendingToolCall::into_tool_call)
            .collect();

        if !text.is_empty() || !calls.is_empty() {
            if !text.is_empty() {
                outcome.text.push_str(&text);
            }
            history.push(Message::assistant_with(text, calls.clone()));
        }

        if calls.is_empty() {
            outcome.steps = turn;
            on_event(AgentEvent::TurnFinished { turn });
            on_event(AgentEvent::AgentFinished { steps: turn });
            if let Some(persistence) = persistence {
                persistence.save(history.messages());
            }
            return Ok(outcome);
        }

        for call in calls {
            on_event(AgentEvent::ToolCallStarted {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });

            let mutating = tools
                .get(&call.name)
                .map(|tool| tool.is_mutating())
                .unwrap_or(true);
            // Selesaikan Ask lebih dulu: pre-hook harus melihat keputusan
            // FINAL, bukan hanya Allow awal — kalau tidak, pre-hook bisa
            // di-bypass di mode ask setelah user menyetujui.
            // `decision_was_asked` menandai keputusan yang lahir dari approval
            // interaktif, supaya trace bisa membedakan `always` dari `allow`.
            let (decision, decision_was_asked) = match permissions
                .decide(&call.name, &call.arguments, mutating)
                .await
            {
                Decision::Ask(reason) => (
                    permissions
                        .resolve_ask(&call.name, &call.arguments, &reason)
                        .await,
                    true,
                ),
                other => (other, false),
            };

            // Pre-hook: deny → tool tidak dijalankan.
            if matches!(decision, Decision::Allow)
                && let Err(denied) = hooks.pre_tool(&call.name, &call.arguments, &tool_ctx.cwd)
            {
                // Hook menolak: laporkan sebagai keputusan `deny` agar UI/trace
                // bisa membedakan "ditolak hook" dari "ditolak policy".
                on_event(AgentEvent::ToolDecided {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    decision: DecisionKind::Deny,
                    reason: format!("blocked by pre-hook: {denied}"),
                });
                let result = ToolResult::err(
                    call.id.clone(),
                    call.name.clone(),
                    format!("hook pre denied: {denied}"),
                );
                on_event(AgentEvent::ToolCallFinished {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    is_error: true,
                    duration_ms: 0,
                    result_preview: preview(&result.content),
                    result_detail: detail(&result.content),
                });
                outcome.tool_calls += 1;
                history.push(Message::tool_result(result));
                continue;
            }

            let started = Instant::now();
            // Laporkan keputusan final sebelum eksekusi (audit/trace). Bila
            // keputusan lahir dari mode `ask` dan diizinkan, pengguna memang
            // memilih "selalu" — laporkan `Always` supaya jejak jelas.
            let (decision_kind, decision_reason) = match &decision {
                Decision::Allow if decision_was_asked => (DecisionKind::Always, String::new()),
                Decision::Allow => (DecisionKind::Allow, String::new()),
                Decision::Deny(reason) => (DecisionKind::Deny, reason.clone()),
                Decision::Ask(reason) => {
                    (DecisionKind::Deny, format!("unresolved approval: {reason}"))
                }
            };
            on_event(AgentEvent::ToolDecided {
                id: call.id.clone(),
                name: call.name.clone(),
                decision: decision_kind,
                reason: decision_reason,
            });
            let result = match decision {
                Decision::Allow => execute_tool(tools, &call, tool_ctx).await,
                Decision::Deny(reason) => ToolResult::err(
                    call.id.clone(),
                    call.name.clone(),
                    format!("permission denied: {reason}"),
                ),
                Decision::Ask(_) => ToolResult::err(
                    call.id.clone(),
                    call.name.clone(),
                    "permission denied: unresolved approval".to_string(),
                ),
            };
            let duration_ms = started.elapsed().as_millis() as u64;

            on_event(AgentEvent::ToolCallFinished {
                id: call.id.clone(),
                name: call.name.clone(),
                is_error: result.is_error,
                duration_ms,
                result_preview: preview(&result.content),
                result_detail: detail(&result.content),
            });
            outcome.tool_calls += 1;
            let exit_code = if result.is_error { 1 } else { 0 };
            history.push(Message::tool_result(result));
            if let Some(warning) =
                hooks.post_tool(&call.name, &call.arguments, &tool_ctx.cwd, exit_code)
            {
                on_event(AgentEvent::Note {
                    text: format!("post-hook: {warning}"),
                });
            }
        }

        outcome.steps = turn;
        on_event(AgentEvent::TurnFinished { turn });
        // Streaming persistensi: simpan tiap akhir turn, bukan hanya di akhir
        // run_agent — kalau proses mati/gagal di tengah, kerja tidak hilang.
        if let Some(persist) = persistence {
            persist.save(history.messages());
        }
    }

    let message = format!("max steps ({}) exceeded", options.max_steps);
    on_event(AgentEvent::Error {
        message: message.clone(),
    });
    Err(AgentError::MaxSteps(message))
}

async fn execute_tool(tools: &ToolRegistry, call: &ToolCall, tool_ctx: &ToolContext) -> ToolResult {
    match tools.get(&call.name) {
        Some(tool) => match tool.execute(call.arguments.clone(), tool_ctx).await {
            Ok(mut result) => {
                result.tool_call_id = call.id.clone();
                result.name = call.name.clone();
                result
            }
            Err(error) => ToolResult::err(call.id.clone(), call.name.clone(), format!("{error}")),
        },
        None => ToolResult::err(
            call.id.clone(),
            call.name.clone(),
            format!("unknown tool: {}", call.name),
        ),
    }
}

/// Cek cepat apakah pesan terakhir adalah hasil tool (predikat bantu untuk host).
pub fn last_is_tool(history: &ContextStore) -> bool {
    matches!(history.last(), Some(m) if m.role == Role::Tool)
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn preview_is_single_line_and_short() {
        let text = "\n\nfirst real line\nsecond line\n";
        assert_eq!(preview(text), "first real line");
        let long = "x".repeat(300);
        let p = preview(&long);
        assert_eq!(p.chars().count(), 161, "160 chars + ellipsis");
        assert!(p.ends_with('…'));
    }

    #[test]
    fn detail_keeps_multi_line_output() {
        let text = "line1\nline2\nline3\n";
        assert_eq!(detail(text), "line1\nline2\nline3");
    }

    #[test]
    fn detail_is_capped_for_huge_output() {
        let huge = "y".repeat(DETAIL_CAP_CHARS + 5_000);
        let d = detail(&huge);
        assert!(d.chars().count() <= DETAIL_CAP_CHARS + 30);
        assert!(d.ends_with("[detail truncated]"));
    }
}
