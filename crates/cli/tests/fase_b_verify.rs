//! Fase B: verification loop.
//!
//! Sebelum perbaikan, tidak ada satu pun jalur yang menjalankan test/check
//! setelah agen mengubah berkas — model bisa mengklaim selesai atas kode yang
//! tidak dikompilasi. Test ini membuktikan hasil verifikasi benar-benar
//! **sampai ke model** (history), bukan sekadar tercetak ke pengguna.

use futures_core::future::BoxFuture;
use minic_core::agent::{NoopHooks, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::AllowAll;
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{Tool, ToolContext, ToolRegistry};
use minic_core::types::{Message, Role, ToolResult};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Tool yang "mengubah berkas" lalu melaporkan sukses.
struct Mutator;

impl Tool for Mutator {
    fn name(&self) -> &str {
        "mutate"
    }
    fn description(&self) -> &str {
        "pretends to edit a file"
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
        Box::pin(async move { Ok(ToolResult::ok(String::new(), "mutate", "edited a.rs")) })
    }
}

/// Tool `bash` palsu: mencatat perintah yang diminta dan membalas sesuai script.
struct FakeBash {
    /// Apakah perintah verifikasi harus gagal.
    fail_verify: bool,
    ran: Arc<Mutex<Vec<String>>>,
}

impl Tool for FakeBash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "runs a command"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"}}})
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            self.ran.lock().unwrap().push(command);
            if self.fail_verify {
                Ok(ToolResult::err(
                    String::new(),
                    "bash",
                    "error[E0308]: mismatched types\n --> src/a.rs:2:5",
                ))
            } else {
                Ok(ToolResult::ok(
                    String::new(),
                    "bash",
                    "Finished test profile",
                ))
            }
        })
    }
}

/// Provider: satu tool call `mutate`, lalu (setelah tool result) teks final.
struct Scripted {
    fired: Arc<Mutex<bool>>,
}

impl ModelProvider for Scripted {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let has_tool_result = req.messages.iter().any(|m| m.role == Role::Tool);
        if has_tool_result {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta("all done".into())));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        } else {
            *self.fired.lock().unwrap() = true;
            let _ = tx.try_send(Ok(ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("m1".into()),
                name: Some("mutate".into()),
                args_fragment: "{}".into(),
            }));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        }
        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

struct Setup {
    registry: ToolRegistry,
    history: ContextStore,
    ctx: ToolContext,
    fired: Arc<Mutex<bool>>,
    ran: Arc<Mutex<Vec<String>>>,
}

fn setup(fail_verify: bool) -> Setup {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(Mutator));
    let ran = Arc::new(Mutex::new(Vec::new()));
    registry.register(Box::new(FakeBash {
        fail_verify,
        ran: Arc::clone(&ran),
    }));
    Setup {
        registry,
        history: {
            let mut h = ContextStore::new();
            h.push(Message::user("change the code"));
            h
        },
        ctx: ToolContext {
            cwd: std::env::temp_dir(),
        },
        fired: Arc::new(Mutex::new(false)),
        ran,
    }
}

async fn run(setup: &mut Setup, verify: Option<&str>) {
    let provider = Scripted {
        fired: Arc::clone(&setup.fired),
    };
    let mut opts = RunOptions::new("test-model");
    opts.max_steps = 4;
    opts.verify_command = verify.map(|s| s.to_string());
    run_agent(
        &provider,
        &setup.registry,
        &AllowAll,
        &NoopHooks,
        &mut setup.history,
        &setup.ctx,
        &opts,
        None,
        &mut |_| {},
    )
    .await
    .expect("run completes");
}

fn tool_texts(history: &ContextStore) -> Vec<String> {
    history
        .messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.text())
        .collect()
}

#[tokio::test]
async fn verification_runs_after_a_successful_edit() {
    let mut s = setup(false);
    run(&mut s, Some("cargo test")).await;

    let ran = s.ran.lock().unwrap().clone();
    assert!(
        ran.iter().any(|c| c == "cargo test"),
        "perintah verifikasi harus dijalankan, dapat: {ran:?}"
    );
}

#[tokio::test]
async fn a_failing_verification_is_visible_to_the_model() {
    let mut s = setup(true);
    run(&mut s, Some("cargo test")).await;

    let texts = tool_texts(&s.history);
    let verify = texts
        .iter()
        .find(|t| t.contains("verification"))
        .unwrap_or_else(|| panic!("hasil verifikasi harus masuk history: {texts:?}"));
    assert!(verify.contains("verification FAILED"), "{verify}");
    // Pesan error nyata ikut terbawa, bukan hanya kata "failed".
    assert!(verify.contains("E0308"), "{verify}");
    assert!(verify.contains("Fix the failure"), "{verify}");
}

#[tokio::test]
async fn a_passing_verification_is_reported_as_passed() {
    let mut s = setup(false);
    run(&mut s, Some("cargo test")).await;
    let texts = tool_texts(&s.history);
    assert!(
        texts.iter().any(|t| t.contains("verification passed")),
        "{texts:?}"
    );
}

#[tokio::test]
async fn without_verify_option_nothing_extra_runs() {
    let mut s = setup(false);
    run(&mut s, None).await;
    assert!(
        s.ran.lock().unwrap().is_empty(),
        "tanpa --verify tidak ada perintah tambahan"
    );
    let texts = tool_texts(&s.history);
    assert!(
        !texts.iter().any(|t| t.contains("verification")),
        "{texts:?}"
    );
}

#[tokio::test]
async fn verification_does_not_run_when_nothing_was_mutated() {
    // Tool read-only → tidak ada yang perlu diverifikasi.
    let mut registry = ToolRegistry::new();
    let ran = Arc::new(Mutex::new(Vec::new()));
    registry.register(Box::new(FakeBash {
        fail_verify: false,
        ran: Arc::clone(&ran),
    }));
    let mut history = ContextStore::new();
    history.push(Message::user("look around"));
    let ctx = ToolContext {
        cwd: std::env::temp_dir(),
    };

    struct ReadOnlyProvider;
    impl ModelProvider for ReadOnlyProvider {
        fn stream(
            &self,
            _req: StreamRequest,
        ) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta("nothing to do".into())));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
            drop(tx);
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
        }
    }

    let mut opts = RunOptions::new("test-model");
    opts.verify_command = Some("cargo test".to_string());
    run_agent(
        &ReadOnlyProvider,
        &registry,
        &AllowAll,
        &NoopHooks,
        &mut history,
        &ctx,
        &opts,
        None,
        &mut |_| {},
    )
    .await
    .expect("run completes");

    assert!(
        ran.lock().unwrap().is_empty(),
        "tanpa mutasi, verifikasi tidak boleh jalan"
    );
}
