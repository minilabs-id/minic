//! Fase D: batch tool read-only berjalan paralel, mutating tetap serial.
//!
//! Sebelumnya kernel menjalankan `for call in calls { ... }` secara serial,
//! sehingga tiga `read_file` memakan waktu tiga kali lipat. Dua hal yang
//! harus dijaga saat memparalelkan:
//!
//! 1. **Hasil tetap urut.** History harus deterministik: hasil tool ke-i
//!    selalu milik call ke-i, bukan milik yang selesai lebih dulu.
//! 2. **Mutating tetap serial.** Dua tulisan tidak boleh beradu, dan approval
//!    tidak boleh muncul dua sekaligus.

use futures_core::future::BoxFuture;
use minic_core::agent::{NoopHooks, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::{AllowAll, PermissionMode};
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{Tool, ToolContext, ToolRegistry};
use minic_core::types::{Message, Role, ToolResult};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Tool read-only yang tidur sesuai argumen `ms`, lalu mengembalikan labelnya.
/// Mencatat berapa banyak yang berjalan bersamaan.
struct SlowRead {
    label: &'static str,
    concurrent: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Tool for SlowRead {
    fn name(&self) -> &str {
        self.label
    }
    fn description(&self) -> &str {
        "reads slowly"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"ms":{"type":"integer"}}})
    }
    fn execute<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let now = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            let ms = args.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            self.concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolResult::ok(
                String::new(),
                self.label,
                format!("result-from-{}", self.label),
            ))
        })
    }
}

/// Tool mutating yang mencatat urutan eksekusi.
struct SlowWrite {
    label: &'static str,
    order: Arc<Mutex<Vec<String>>>,
    concurrent: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Tool for SlowWrite {
    fn name(&self) -> &str {
        self.label
    }
    fn description(&self) -> &str {
        "writes slowly"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let now = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.order.lock().unwrap().push(self.label.to_string());
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolResult::ok(
                String::new(),
                self.label,
                format!("wrote-{}", self.label),
            ))
        })
    }
}

/// Provider yang meminta beberapa tool call sekaligus pada giliran pertama.
struct BatchProvider {
    calls: Vec<(&'static str, Value)>,
}

impl ModelProvider for BatchProvider {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let done = req.messages.iter().any(|m| m.role == Role::Tool);
        if done {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta("done".into())));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        } else {
            for (index, (name, args)) in self.calls.iter().enumerate() {
                let _ = tx.try_send(Ok(ProviderEvent::ToolCallDelta {
                    index,
                    id: Some(format!("c{index}")),
                    name: Some((*name).to_string()),
                    args_fragment: args.to_string(),
                }));
            }
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::ToolCalls)));
        }
        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

struct Harness {
    registry: ToolRegistry,
    history: ContextStore,
    ctx: ToolContext,
    peak: Arc<AtomicUsize>,
    order: Arc<Mutex<Vec<String>>>,
}

fn harness(read_tools: &[&'static str], write_tools: &[&'static str]) -> Harness {
    let concurrent = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    for label in read_tools {
        registry.register(Box::new(SlowRead {
            label,
            concurrent: Arc::clone(&concurrent),
            peak: Arc::clone(&peak),
        }));
    }
    for label in write_tools {
        registry.register(Box::new(SlowWrite {
            label,
            order: Arc::clone(&order),
            concurrent: Arc::clone(&concurrent),
            peak: Arc::clone(&peak),
        }));
    }
    Harness {
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
        peak,
        order,
    }
}

async fn run(h: &mut Harness, calls: Vec<(&'static str, Value)>) -> minic_core::agent::RunOutcome {
    let provider = BatchProvider { calls };
    let mut opts = RunOptions::new("m");
    opts.max_steps = 3;
    run_agent(
        &provider,
        &h.registry,
        &AllowAll,
        &NoopHooks,
        &mut h.history,
        &h.ctx,
        &opts,
        None,
        &mut |_| {},
    )
    .await
    .expect("run completes")
}

fn tool_results(history: &ContextStore) -> Vec<String> {
    history
        .messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.text())
        .collect()
}

#[tokio::test]
async fn read_only_calls_run_concurrently() {
    let mut h = harness(&["r1", "r2", "r3"], &[]);
    let started = Instant::now();
    run(
        &mut h,
        vec![
            ("r1", json!({"ms": 200})),
            ("r2", json!({"ms": 200})),
            ("r3", json!({"ms": 200})),
        ],
    )
    .await;
    let elapsed = started.elapsed();

    assert!(
        h.peak.load(Ordering::SeqCst) >= 2,
        "tool read-only harus berjalan bersamaan (peak={})",
        h.peak.load(Ordering::SeqCst)
    );
    // Bukti paralelisme diambil dari `peak` di atas, bukan dari waktu dinding:
    // ambang waktu membuat test gagal acak di mesin yang sedang sibuk. Waktu
    // tetap dicetak untuk diagnosa, dan batas longgarnya hanya menjaga agar
    // eksekusi nyata-benar terjadi tiga kali, bukan nol kali.
    assert!(
        elapsed >= Duration::from_millis(150),
        "tiga baca 200ms tidak mungkin selesai {elapsed:?}"
    );
}

#[tokio::test]
async fn parallel_results_keep_call_order() {
    let mut h = harness(&["r1", "r2", "r3"], &[]);
    // Durasi dibalik: r1 paling lambat. Hasil harus tetap urut r1,r2,r3.
    run(
        &mut h,
        vec![
            ("r1", json!({"ms": 150})),
            ("r2", json!({"ms": 80})),
            ("r3", json!({"ms": 10})),
        ],
    )
    .await;

    assert_eq!(
        tool_results(&h.history),
        vec!["result-from-r1", "result-from-r2", "result-from-r3"],
        "history harus urut sesuai call, bukan urutan selesai"
    );
}

#[tokio::test]
async fn tool_result_ids_match_their_calls() {
    let mut h = harness(&["r1", "r2"], &[]);
    run(
        &mut h,
        vec![("r1", json!({"ms": 100})), ("r2", json!({"ms": 10}))],
    )
    .await;

    let ids: Vec<String> = h
        .history
        .messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.tool_call_id.clone().unwrap_or_default())
        .collect();
    assert_eq!(ids, vec!["c0", "c1"]);
}

#[tokio::test]
async fn mutating_calls_stay_serial_in_request_order() {
    let mut h = harness(&[], &["w1", "w2", "w3"]);
    run(
        &mut h,
        vec![("w1", json!({})), ("w2", json!({})), ("w3", json!({}))],
    )
    .await;

    assert_eq!(
        h.order.lock().unwrap().clone(),
        vec!["w1", "w2", "w3"],
        "mutasi harus serial dan berurutan"
    );
    assert_eq!(
        h.peak.load(Ordering::SeqCst),
        1,
        "tidak boleh ada dua mutasi bersamaan"
    );
}

#[tokio::test]
async fn a_mutating_call_breaks_the_parallel_batch() {
    // r1, w1, r2 → tiga batch terpisah: [r1], [w1], [r2].
    let mut h = harness(&["r1", "r2"], &["w1"]);
    run(
        &mut h,
        vec![
            ("r1", json!({"ms": 10})),
            ("w1", json!({})),
            ("r2", json!({"ms": 10})),
        ],
    )
    .await;

    assert_eq!(
        tool_results(&h.history),
        vec!["result-from-r1", "wrote-w1", "result-from-r2"],
        "urutan campuran baca/tulis harus terjaga"
    );
    assert_eq!(
        h.peak.load(Ordering::SeqCst),
        1,
        "batch dipisah oleh mutasi → tidak ada tumpang tindih"
    );
}

#[tokio::test]
async fn unknown_tool_is_treated_as_serial_and_fails_safely() {
    let mut h = harness(&["r1"], &[]);
    run(&mut h, vec![("nope", json!({}))]).await;
    let results = tool_results(&h.history);
    assert!(
        results.iter().any(|t| t.contains("unknown tool")),
        "{results:?}"
    );
}
