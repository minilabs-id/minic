//! Fase A: integritas respons model.
//!
//! Bug yang dijaga di sini (terbukti empiris sebelum perbaikan): kernel
//! membuang `FinishReason` dan menelan kegagalan parse argumen menjadi
//! `Value::Null`, sehingga tool tetap **dieksekusi** dari respons yang
//! terpotong — model lalu disalahkan atas kesalahan transport.
//!
//! Test di bawah mengunci tiga hal:
//! 1. respons terpotong (`finish_reason=length`) tidak mengeksekusi tool apa pun;
//! 2. argumen rusak dilaporkan sebagai error yang menyebut sebabnya, tanpa eksekusi;
//! 3. jalur normal (`stop`, argumen kosong sah) tidak ikut terblokir.

use futures_core::future::BoxFuture;
use minic_core::agent::{NoopHooks, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::events::AgentEvent;
use minic_core::history::ContextStore;
use minic_core::permission::{AllowAll, PermissionMode};
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{Tool, ToolContext, ToolRegistry};
use minic_core::types::{Message, Role, ToolResult};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Tool mata-mata: menghitung berapa kali ia benar-benar dieksekusi.
struct SpyTool {
    hits: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Tool for SpyTool {
    fn name(&self) -> &str {
        "spy"
    }

    fn description(&self) -> &str {
        "records invocations"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"}}})
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            self.hits.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(args.to_string());
            Ok(ToolResult::ok(String::new(), "spy", "spy ok"))
        })
    }
}

/// Provider yang memancarkan satu tool call dengan fragmen argumen tertentu,
/// lalu `finish`.
struct ScriptedProvider {
    fragment: &'static str,
    finish: FinishReason,
    /// Bila false, argumen dikirim kosong (tool tanpa parameter).
    with_args: bool,
}

impl ModelProvider for ScriptedProvider {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let already_ran = req.messages.iter().any(|m| m.role == Role::Tool);
        let fragment = self.fragment;
        let finish = self.finish;
        let with_args = self.with_args;
        if already_ran {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta("done".into())));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        } else {
            let _ = tx.try_send(Ok(ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("c1".into()),
                name: Some("spy".into()),
                args_fragment: if with_args {
                    fragment.to_string()
                } else {
                    String::new()
                },
            }));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(finish)));
        }
        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

struct Harness {
    hits: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    registry: ToolRegistry,
    history: ContextStore,
    ctx: ToolContext,
}

fn harness() -> Harness {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SpyTool {
        hits: Arc::clone(&hits),
        seen: Arc::clone(&seen),
    }));
    Harness {
        hits,
        seen,
        registry,
        history: {
            let mut h = ContextStore::new();
            h.push(Message::user("go"));
            h
        },
        ctx: ToolContext {
            cwd: std::env::temp_dir(),
            permission_mode: PermissionMode::Auto,
        },
    }
}

async fn run(
    harness: &mut Harness,
    provider: &ScriptedProvider,
) -> (minic_core::agent::RunOutcome, Vec<AgentEvent>) {
    let mut events = Vec::new();
    let mut opts = RunOptions::new("test-model");
    opts.max_steps = 3;
    let outcome = run_agent(
        provider,
        &harness.registry,
        &AllowAll,
        &NoopHooks,
        &mut harness.history,
        &harness.ctx,
        &opts,
        None,
        &mut |event| events.push(event),
    )
    .await
    .expect("run should complete");
    (outcome, events)
}

#[tokio::test]
async fn truncated_response_never_executes_tool_calls() {
    let mut h = harness();
    let provider = ScriptedProvider {
        // JSON terpotong, persis seperti output yang dipotong `max_tokens`.
        fragment: "{\"command\": \"git commit -m 'hal",
        finish: FinishReason::Length,
        with_args: true,
    };

    let (outcome, events) = run(&mut h, &provider).await;

    assert_eq!(
        h.hits.load(Ordering::SeqCst),
        0,
        "tool TIDAK boleh dieksekusi dari respons terpotong"
    );
    assert!(h.seen.lock().unwrap().is_empty());
    assert!(outcome.truncated, "outcome harus menandai truncation");

    // Pengguna/model harus diberi tahu sebabnya, bukan pesan menyesatkan.
    let note = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Note { text } => Some(text.clone()),
            _ => None,
        })
        .expect("harus ada Note yang menjelaskan truncation");
    assert!(note.contains("finish_reason=length"), "note: {note}");
    assert!(note.contains("discarded"), "note: {note}");
}

#[tokio::test]
async fn truncated_response_keeps_history_consistent() {
    let mut h = harness();
    let provider = ScriptedProvider {
        fragment: "{\"command\": \"hal",
        finish: FinishReason::Length,
        with_args: true,
    };
    let _ = run(&mut h, &provider).await;

    // Tidak boleh ada tool_call yang menggantung tanpa tool_result — provider
    // akan menolak pasangan tidak lengkap pada request berikutnya.
    for message in h.history.messages() {
        assert!(
            message.tool_calls.is_empty(),
            "respons terpotong tidak boleh menyimpan tool call"
        );
    }
}

#[tokio::test]
async fn malformed_arguments_are_reported_and_not_executed() {
    let mut h = harness();
    let provider = ScriptedProvider {
        fragment: "{ this is not json",
        finish: FinishReason::Stop,
        with_args: true,
    };

    let (_, events) = run(&mut h, &provider).await;

    assert_eq!(
        h.hits.load(Ordering::SeqCst),
        0,
        "argumen rusak tidak boleh dieksekusi"
    );

    // Model harus menerima penjelasan sebenarnya beserta petunjuk perbaikan.
    let tool_result = h
        .history
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .map(|m| m.text())
        .expect("hasil error untuk argumen rusak");
    assert!(
        tool_result.contains("malformed tool arguments"),
        "harus menyebut malformed, bukan 'missing command': {tool_result}"
    );
    assert!(tool_result.contains("was NOT executed"), "{tool_result}");

    // Keputusan deny tercatat agar terlihat di UI/trace.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolDecided { reason, .. } if reason.contains("malformed")
    )));
}

#[tokio::test]
async fn valid_arguments_still_execute_normally() {
    let mut h = harness();
    let provider = ScriptedProvider {
        fragment: "{\"command\": \"ls\"}",
        finish: FinishReason::Stop,
        with_args: true,
    };

    let (outcome, _) = run(&mut h, &provider).await;

    assert_eq!(h.hits.load(Ordering::SeqCst), 1, "jalur normal tetap jalan");
    assert!(!outcome.truncated);
    assert_eq!(
        h.seen.lock().unwrap().as_slice(),
        &["{\"command\":\"ls\"}".to_string()]
    );
}

#[tokio::test]
async fn empty_arguments_remain_valid_for_parameterless_tools() {
    let mut h = harness();
    let provider = ScriptedProvider {
        fragment: "",
        finish: FinishReason::Stop,
        with_args: false,
    };

    let (_, _) = run(&mut h, &provider).await;

    // Argumen kosong sah (tool tanpa parameter) dan tidak boleh dianggap rusak.
    assert_eq!(h.hits.load(Ordering::SeqCst), 1);
    assert_eq!(h.seen.lock().unwrap().as_slice(), &["null".to_string()]);
}

#[tokio::test]
async fn content_filter_finish_does_not_execute_tools() {
    // Alasan selain `length` juga berarti respons tidak lengkap/sah.
    let mut h = harness();
    let provider = ScriptedProvider {
        fragment: "{\"command\": \"hal",
        finish: FinishReason::ContentFilter,
        with_args: true,
    };
    let _ = run(&mut h, &provider).await;
    // Argumen tetap terpotong → tidak dieksekusi (dilaporkan sebagai malformed).
    assert_eq!(h.hits.load(Ordering::SeqCst), 0);
}
