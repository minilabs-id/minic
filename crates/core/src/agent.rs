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
    /// Perintah verifikasi yang dijalankan lewat `bash` setelah batch tool
    /// **mutating** yang berhasil (mis. `cargo check`). Hasilnya disuntikkan ke
    /// history sebagai tool result, sehingga model melihat dan memperbaiki
    /// sendiri — bukan hanya dicetak ke pengguna.
    pub verify_command: Option<String>,
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
            verify_command: None,
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
    /// True bila run berhenti karena respons model terpotong
    /// (`finish_reason=length`) dan tool call-nya dibuang.
    pub truncated: bool,
}

/// Akumulator satu tool call selama streaming (argumen datang bertahap).
#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    args: String,
}

/// Hasil konversi satu `PendingToolCall` yang sudah selesai di-stream.
enum PendingOutcome {
    /// Siap dieksekusi.
    Ready(ToolCall),
    /// Argumen tidak bisa di-parse. **Tool tidak boleh dieksekusi**: argumen
    /// rusak hampir selalu berarti output model terpotong, dan mengeksekusi
    /// tool dengan `Null` menyalahkan model atas kesalahan transport.
    Malformed {
        id: String,
        name: String,
        reason: String,
        raw: String,
    },
    /// Tidak ada nama tool: bukan panggilan yang sah, abaikan.
    Unusable,
}

impl PendingToolCall {
    fn into_outcome(self) -> PendingOutcome {
        let Some(name) = self.name else {
            return PendingOutcome::Unusable;
        };
        let id = self.id.unwrap_or_else(next_call_id);

        // Argumen kosong **sah** (tool tanpa parameter). Argumen yang tidak
        // kosong tetapi gagal di-parse adalah kegagalan nyata dan harus
        // dilaporkan, bukan ditelan menjadi `Value::Null`.
        if self.args.trim().is_empty() {
            return PendingOutcome::Ready(ToolCall {
                id,
                name,
                arguments: serde_json::Value::Null,
            });
        }
        match serde_json::from_str(&self.args) {
            Ok(arguments) => PendingOutcome::Ready(ToolCall {
                id,
                name,
                arguments,
            }),
            Err(error) => PendingOutcome::Malformed {
                id,
                name,
                reason: error.to_string(),
                raw: truncate_raw(&self.args, 200),
            },
        }
    }
}

/// Cuplikan argumen mentah untuk pesan error (cukup untuk diagnosis, tidak
/// membanjiri konteks).
fn truncate_raw(raw: &str, max_chars: usize) -> String {
    let mut chars = raw.chars();
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
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
        // Alasan stream berhenti. Dulu dibuang; tanpa ini, respons yang dipotong
        // server tidak bisa dibedakan dari respons yang selesai normal.
        let mut finish_reason = crate::provider::FinishReason::Unknown;

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
                ProviderEvent::Finish(reason) => finish_reason = reason,
            }
        }

        // Respons terpotong: tool call apa pun di sini **tidak lengkap** dan
        // tidak boleh dieksekusi. Tanpa gerbang ini, tool berjalan dengan
        // argumen `Null` dan model disalahkan atas kesalahan transport
        // (terbukti: tool dieksekusi dengan command=<MISSING>).
        let truncated = matches!(finish_reason, crate::provider::FinishReason::Length);
        if truncated && !pending.is_empty() {
            let dropped = pending.len();
            if !text.is_empty() {
                outcome.text.push_str(&text);
            }
            // Simpan teks parsial tanpa tool call: history tetap valid (tidak
            // ada tool_call yang menggantung tanpa hasil).
            history.push(Message::assistant_with(text, Vec::new()));
            on_event(AgentEvent::Note {
                text: format!(
                    "response truncated (finish_reason=length): {dropped} tool call(s) discarded \
                     because their arguments are incomplete — raise --max-output-tokens or split \
                     the task into smaller steps"
                ),
            });
            outcome.steps = turn;
            outcome.truncated = true;
            on_event(AgentEvent::TurnFinished { turn });
            on_event(AgentEvent::AgentFinished { steps: turn });
            if let Some(persistence) = persistence {
                persistence.save(history.messages());
            }
            return Ok(outcome);
        }

        // Pisahkan panggilan yang sah dari yang argumennya rusak. Yang rusak
        // dilaporkan sebagai hasil tool error **tanpa dieksekusi**, supaya model
        // melihat penyebab sebenarnya (parse error) dan bisa memperbaikinya.
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut malformed: Vec<(String, String, String, String)> = Vec::new();
        for item in pending {
            match item.into_outcome() {
                PendingOutcome::Ready(call) => calls.push(call),
                PendingOutcome::Malformed {
                    id,
                    name,
                    reason,
                    raw,
                } => malformed.push((id, name, reason, raw)),
                PendingOutcome::Unusable => {}
            }
        }

        if !text.is_empty() || !calls.is_empty() || !malformed.is_empty() {
            if !text.is_empty() {
                outcome.text.push_str(&text);
            }
            let mut recorded = calls.clone();
            for (id, name, _, _) in &malformed {
                recorded.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: serde_json::Value::Null,
                });
            }
            history.push(Message::assistant_with(text, recorded));
        }

        // Hasil error untuk argumen rusak (tanpa eksekusi).
        for (id, name, reason, raw) in malformed {
            on_event(AgentEvent::ToolCallStarted {
                id: id.clone(),
                name: name.clone(),
                arguments: serde_json::Value::Null,
            });
            let message = format!(
                "malformed tool arguments from model: {reason}\nraw (truncated): {raw}\n\
                 The tool was NOT executed. Re-issue the call with complete JSON."
            );
            on_event(AgentEvent::ToolDecided {
                id: id.clone(),
                name: name.clone(),
                decision: DecisionKind::Deny,
                reason: "malformed arguments (not executed)".to_string(),
            });
            let result = ToolResult::err(id.clone(), name.clone(), message);
            history.push(Message::tool_result(result.clone()));
            outcome.tool_calls += 1;
            on_event(AgentEvent::ToolCallFinished {
                id,
                name,
                is_error: true,
                duration_ms: 0,
                result_preview: preview(&result.content),
                result_detail: detail(&result.content),
            });
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

        // Apakah turn ini melakukan mutasi yang berhasil? Hanya itu yang perlu
        // diverifikasi — memverifikasi setelah `read_file` hanya membakar step.
        let mut mutated = false;

        // Batch tool call: deretan tool **read-only** yang berurutan dijalankan
        // paralel. Tool mutating dan tool yang butuh approval tetap serial —
        // satu modal approval pada satu waktu, dan urutan tulis tidak boleh
        // ditukar. Hasil dikembalikan sesuai urutan asli `calls` supaya
        // history tetap deterministik (dua run dengan respons sama = history
        // sama).
        for batch in batches(&calls, tools) {
            if batch.len() > 1 {
                let results = run_parallel(&batch, tools, tool_ctx).await;
                for (call, result) in batch.iter().zip(results) {
                    on_event(AgentEvent::ToolCallStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                    on_event(AgentEvent::ToolDecided {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        decision: DecisionKind::Allow,
                        reason: String::new(),
                    });
                    on_event(AgentEvent::ToolCallFinished {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        is_error: result.is_error,
                        duration_ms: 0,
                        result_preview: preview(&result.content),
                        result_detail: detail(&result.content),
                    });
                    outcome.tool_calls += 1;
                    let failed = result.is_error;
                    let name = call.name.clone();
                    let args = call.arguments.clone();
                    history.push(Message::tool_result(result));
                    if let Some(warning) =
                        hooks.post_tool(&name, &args, &tool_ctx.cwd, failed as i32)
                    {
                        on_event(AgentEvent::Note {
                            text: format!("post-hook: {warning}"),
                        });
                    }
                }
                continue;
            }
            let call = batch.into_iter().next().expect("batch tidak pernah kosong");

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
            if mutating
                && !result.is_error
                && matches!(decision_kind, DecisionKind::Allow | DecisionKind::Always)
            {
                mutated = true;
            }
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

        // Verification loop: setelah ada mutasi yang berhasil, jalankan
        // perintah verifikasi dan **suntikkan hasilnya ke history** — model
        // harus melihat kerusakan yang ia buat sendiri, bukan manusia yang
        // menemukannya belakangan.
        if mutated && let Some(command) = options.verify_command.as_deref() {
            let verify = run_verify(command, tools, tool_ctx).await;
            let failed = verify.is_error;
            on_event(AgentEvent::ToolCallStarted {
                id: verify.tool_call_id.clone().to_string(),
                name: "verify".to_string(),
                arguments: serde_json::json!({ "command": command }),
            });
            on_event(AgentEvent::ToolDecided {
                id: verify.tool_call_id.clone().to_string(),
                name: "verify".to_string(),
                decision: DecisionKind::Always,
                reason: format!("verification after mutation: {command}"),
            });
            on_event(AgentEvent::ToolCallFinished {
                id: verify.tool_call_id.clone().to_string(),
                name: "verify".to_string(),
                is_error: failed,
                duration_ms: 0,
                result_preview: preview(&verify.content),
                result_detail: detail(&verify.content),
            });
            if failed {
                on_event(AgentEvent::Note {
                    text: format!("verification FAILED: {command} — see the verify result"),
                });
            }
            history.push(Message::tool_result(verify));
            outcome.tool_calls += 1;
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

/// Pecah daftar tool call menjadi batch eksekusi.
///
/// Aturannya: tool yang **aman diparalelkan** adalah tool yang terdaftar,
/// read-only (`!is_mutating()`) dan tidak perlu approval. Selain itu, batch
/// langsung ditutup: superset selalu berisi satu call saja, sehingga:
/// - approval tetap satu per satu (tidak ada dua modal sekaligus),
/// - urutan mutasi terjaga persis seperti yang diminta model.
///
/// Mengembalikan `Vec<Vec<ToolCall>>` dengan urutan yang sama seperti input.
fn batches(calls: &[ToolCall], tools: &ToolRegistry) -> Vec<Vec<ToolCall>> {
    let mut out: Vec<Vec<ToolCall>> = Vec::new();
    let mut current: Vec<ToolCall> = Vec::new();
    for call in calls {
        let parallelizable = tools
            .get(&call.name)
            .map(|tool| !tool.is_mutating())
            .unwrap_or(false); // tool tak dikenal → serial (aman)
        if parallelizable {
            current.push(call.clone());
        } else {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.push(vec![call.clone()]);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Jalankan satu batch read-only secara paralel, kembalikan hasil sesuai
/// urutan batch (bukan urutan selesai), supaya history deterministik.
async fn run_parallel(
    batch: &[ToolCall],
    tools: &ToolRegistry,
    tool_ctx: &ToolContext,
) -> Vec<ToolResult> {
    let futures: Vec<_> = batch
        .iter()
        .map(|call| execute_tool(tools, call, tool_ctx))
        .collect();
    futures_util::future::join_all(futures).await
}

/// Jalankan perintah verifikasi lewat tool `bash` yang sudah terdaftar.
///
/// Kernel tidak mengeksekusi proses sendiri (tetap bebas I/O) — ia memakai
/// tool yang sama dengan yang dipakai model, sehingga sandbox/policy/checkpoint
/// tetap berlaku. Bila `bash` tidak terdaftar, verifikasi dilewati dengan hasil
/// non-error agar run tidak gagal hanya karena fitur opsional ini.
async fn run_verify(command: &str, tools: &ToolRegistry, tool_ctx: &ToolContext) -> ToolResult {
    let call = ToolCall {
        id: next_call_id(),
        name: "bash".to_string(),
        arguments: serde_json::json!({ "command": command }),
    };
    let mut result = execute_tool(tools, &call, tool_ctx).await;
    result.name = "verify".to_string();
    if !result.is_error {
        result.content = format!(
            "verification passed: {command}
{}",
            result.content
        );
        result.content = result.content.trim_end().to_string();
    } else {
        result.content = format!(
            "verification FAILED: {command}
{}

Fix the failure before continuing.",
            result.content.trim_end()
        );
    }
    result
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
