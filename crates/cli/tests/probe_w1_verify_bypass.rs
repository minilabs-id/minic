//! Probe: apakah jalur verifikasi menghormati handler izin?
//!
//! Test ini **sengaja** ditulis untuk membuktikan klaim di `AUDIT_work_mode.md`
//! (W1): `run_verify` memanggil `execute_tool` langsung tanpa
//! `permissions.decide`. Handler di bawah menolak **semua** tool; bila
//! verifikasi tetap berjalan, kebocoran terbukti.
//!
//! Setelah W1 diperbaiki, test ini harus diubah menjadi kebalikannya:
//! verifikasi harus ditolak dan hasilnya bukan "verification passed/failed".

use futures_core::future::BoxFuture;
use minic_core::agent::{NoopHooks, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::{Decision, PermissionHandler, PermissionMode};
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

/// **W1**: verifikasi menjalankan `bash` tanpa bertanya ke handler izin.
///
/// Test ini mengharapkan kebocoran (assertion "bash ran without permission").
/// Saat W1 diperbaiki, ubah menjadi `assert!(!ran)` beserta penolakan yang
/// dilaporkan ke model.
#[tokio::test]
async fn verify_runs_bash_without_consulting_the_permission_handler() {
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
        permission_mode: PermissionMode::Ask,
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

    // Kebocoran yang didokumentasikan di AUDIT_work_mode.md W1:
    assert!(
        bash_ran.load(Ordering::SeqCst),
        "verifikasi TIDAK berjalan — berarti W1 sudah diperbaiki; \
         ubah test ini menjadi assertion kebalikannya"
    );
    assert!(
        !bash_asked.load(Ordering::SeqCst),
        "handler izin ternyata dipanggil untuk bash — W1 sudah diperbaiki"
    );
}
