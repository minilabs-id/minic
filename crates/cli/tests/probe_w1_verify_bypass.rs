//! W1: jalur verifikasi harus menghormati handler izin.
//!
//! Dulu `run_verify` memanggil `execute_tool` langsung, sehingga perintah
//! verifikasi berjalan tanpa approval di mode `ask` dan melewati shell guard.
//! Test ini dulu **lulus** dengan assertion "kebocoran terjadi"; sekarang
//! assertion-nya dibalik dan menjadi penjaga tetap.

use futures_core::future::BoxFuture;
use minic_core::agent::{NoopHooks, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::{Decision, PermissionHandler};
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{Tool, ToolContext, ToolRegistry};
use minic_core::types::{Message, Role, ToolResult};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Handler yang menolak semua, tetapi mengizinkan `mutate` agar jalur
/// verifikasi bisa tercapai (verifikasi butuh satu mutasi sukses lebih dulu).
struct DenyBashOnly {
    bash_asked: Arc<AtomicBool>,
}

impl PermissionHandler for DenyBashOnly {
    fn decide<'a>(
        &'a self,
        tool_name: &'a str,
        _args: &'a Value,
        _mutating: bool,
    ) -> BoxFuture<'a, Decision> {
        let asked = Arc::clone(&self.bash_asked);
        Box::pin(async move {
            if tool_name == "bash" {
                asked.store(true, Ordering::SeqCst);
                return Decision::Deny("bash is never allowed in this probe".to_string());
            }
            // `mutate` diizinkan supaya `mutated` menjadi true dan jalur
            // verifikasi benar-benar dieksekusi.
            Decision::Allow
        })
    }
}

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

/// Tool `bash` yang mencatat bahwa ia benar-benar dijalankan.
struct Bash {
    ran: Arc<AtomicBool>,
}

impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "run a shell command"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"}}})
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        let ran = Arc::clone(&self.ran);
        Box::pin(async move {
            ran.store(true, Ordering::SeqCst);
            Ok(ToolResult::ok(String::new(), "bash", "ran"))
        })
    }
}

struct Scripted {
    fired: Arc<AtomicBool>,
}

impl ModelProvider for Scripted {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let already_ran = req.messages.iter().any(|m| m.role == Role::Tool);
        if already_ran || self.fired.swap(true, Ordering::SeqCst) {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta("done".to_string())));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        } else {
            let _ = tx.try_send(Ok(ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_m".to_string()),
                name: Some("mutate".to_string()),
                args_fragment: "{}".to_string(),
            }));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::ToolCalls)));
        }
        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

/// **W1**: verifikasi bertanya ke handler izin dan menghormati penolakan.
#[tokio::test]
async fn verify_asks_the_permission_handler_and_respects_a_denial() {
    let mut registry = ToolRegistry::new();
    let bash_ran = Arc::new(AtomicBool::new(false));
    registry.register(Box::new(Mutator));
    registry.register(Box::new(Bash {
        ran: Arc::clone(&bash_ran),
    }));

    let bash_asked = Arc::new(AtomicBool::new(false));
    let policy = DenyBashOnly {
        bash_asked: Arc::clone(&bash_asked),
    };

    let ctx = ToolContext {
        cwd: std::env::temp_dir(),
    };
    let mut history = ContextStore::new();
    history.push(Message::user("change the code"));

    let provider = Scripted {
        fired: Arc::new(AtomicBool::new(false)),
    };
    let mut opts = RunOptions::new("test-model");
    opts.max_steps = 4;
    opts.verify_command = Some("echo verify".to_string());

    run_agent(
        &provider,
        &registry,
        &policy,
        &NoopHooks,
        &mut history,
        &ctx,
        &opts,
        None,
        &mut |_| {},
    )
    .await
    .expect("run completes");

    // Handler izin harus **ditanya** dulu…
    assert!(
        bash_asked.load(Ordering::SeqCst),
        "verifikasi tidak berkonsultasi dengan handler izin (kebocoran W1 kembali)"
    );
    // …dan penolakannya dihormati: perintah tidak boleh dijalankan.
    assert!(
        !bash_ran.load(Ordering::SeqCst),
        "perintah verifikasi tetap dijalankan meski ditolak (kebocoran W1 kembali)"
    );

    // Penolakan harus sampai ke model sebagai pesan yang bisa ditindaklanjuti,
    // bukan klaim "verification passed".
    let tool_messages: Vec<String> = history
        .messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.text())
        .collect();
    let verify_message = tool_messages
        .iter()
        .find(|text| text.contains("verification"))
        .unwrap_or_else(|| panic!("tidak ada pesan verifikasi: {tool_messages:?}"));
    assert!(
        verify_message.contains("permission denied"),
        "penolakan harus disebut jelas: {verify_message}"
    );
    assert!(
        !verify_message.contains("verification passed"),
        "tidak boleh mengklaim lulus: {verify_message}"
    );
}
