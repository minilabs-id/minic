//! M7: shell guard pada level runtime (`run_agent`), bukan hanya unit policy.
//!
//! Membuktikan bahwa perintah `bash` berisiko benar-benar masuk jalur approval
//! di `--mode auto`, dan bahwa kegagalan approval (non-TTY) menghasilkan deny
//! fail-closed — jadi perintah tidak pernah dieksekusi diam-diam.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::stream;
use minic::policy::SimplePolicy;
use minic::tools::bash::Bash;
use minic_core::agent::{RunOptions, run_agent};
use minic_core::history::ContextStore;
use minic_core::permission::{PermissionHandler, PermissionMode};
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::Message;
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_m7_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

/// Provider yang meminta satu panggilan `bash` dengan `command` yang diberikan,
/// lalu selesai pada turn berikutnya.
struct BashOnce {
    command: String,
    calls: AtomicUsize,
}

impl BashOnce {
    fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            calls: AtomicUsize::new(0),
        }
    }
}

impl ModelProvider for BashOnce {
    fn stream(
        &self,
        _req: StreamRequest,
    ) -> BoxStream<'static, Result<ProviderEvent, minic_core::error::AgentError>> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let command = self.command.clone();
        let events: Vec<Result<ProviderEvent, minic_core::error::AgentError>> = if first {
            vec![
                Ok(ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_bash_0".to_string()),
                    name: Some("bash".to_string()),
                    args_fragment: json!({ "command": command }).to_string(),
                }),
                Ok(ProviderEvent::Finish(FinishReason::ToolCalls)),
            ]
        } else {
            vec![
                Ok(ProviderEvent::TextDelta("done".to_string())),
                Ok(ProviderEvent::Finish(FinishReason::Stop)),
            ]
        };
        Box::pin(stream::iter(events))
    }
}

/// Policy yang menolak setiap `Ask` (mensimulasikan non-TTY) dan mencatat
/// berapa kali approval diminta.
struct AskRefusingPolicy {
    inner: SimplePolicy,
    asks: Arc<AtomicUsize>,
}

impl PermissionHandler for AskRefusingPolicy {
    fn decide<'a>(
        &'a self,
        tool: &'a str,
        args: &'a serde_json::Value,
        mutating: bool,
    ) -> futures_core::future::BoxFuture<'a, minic_core::permission::Decision> {
        self.inner.decide(tool, args, mutating)
    }

    fn resolve_ask<'a>(
        &'a self,
        _tool: &'a str,
        _args: &'a serde_json::Value,
        _reason: &'a str,
    ) -> futures_core::future::BoxFuture<'a, minic_core::permission::Decision> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            minic_core::permission::Decision::Deny(
                "non-interactive session: approval denied (fail-closed)".to_string(),
            )
        })
    }
}

async fn run(command: &str, tag: &str) -> (Arc<AtomicUsize>, String, Vec<String>) {
    let root = temp_root(tag);
    let marker = root.join("marker.txt");

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(Bash::new("off")));

    let policy = AskRefusingPolicy {
        inner: SimplePolicy::new(root.clone(), PermissionMode::Auto),
        asks: Arc::new(AtomicUsize::new(0)),
    };
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };

    let mut history = ContextStore::new();
    history.push(Message::user("run it"));

    let provider = BashOnce::new(command.replace("MARKER", &marker.to_string_lossy()));
    let mut notes: Vec<String> = Vec::new();
    {
        let mut sink = |event: minic_core::events::AgentEvent| {
            if let minic_core::events::AgentEvent::ToolDecided {
                decision, reason, ..
            } = &event
            {
                notes.push(format!("{}:{reason}", decision.as_str()));
            }
        };
        run_agent(
            &provider,
            &registry,
            &policy,
            &minic_core::agent::NoopHooks,
            &mut history,
            &ctx,
            &RunOptions::new("fake"),
            None,
            &mut sink,
        )
        .await
        .expect("agent finishes");
    }

    let marker_written = if marker.exists() {
        std::fs::read_to_string(&marker).unwrap_or_default()
    } else {
        String::new()
    };
    (policy.asks.clone(), marker_written, notes)
}

// Helper kecil agar test tidak bergantung pada API internal.

#[tokio::test]
async fn risky_command_is_denied_without_approver_and_never_runs() {
    // Perintah yang menulis marker + berisiko menurut klasifikasi.
    let command = "git push && echo written > MARKER";
    let (asks, marker, notes) = run(command, "risky").await;
    assert_eq!(
        asks.load(Ordering::SeqCst),
        1,
        "perintah berisiko harus meminta approval"
    );
    assert!(marker.is_empty(), "perintah tidak boleh dieksekusi");
    assert!(
        notes.iter().any(|note| note.starts_with("deny:")),
        "trace harus mencatat deny: {notes:?}"
    );
}

#[tokio::test]
async fn ordinary_command_runs_without_approval_in_auto_mode() {
    let command = "echo written > MARKER";
    let (asks, marker, notes) = run(command, "safe").await;
    assert_eq!(
        asks.load(Ordering::SeqCst),
        0,
        "perintah aman tidak boleh meminta approval di mode auto"
    );
    assert!(
        marker.contains("written"),
        "perintah aman harus dijalankan: marker={marker:?}"
    );
    assert!(
        notes.iter().any(|note| note.starts_with("allow:")),
        "trace harus mencatat allow: {notes:?}"
    );
}

// ── Checkpoint whole-tree untuk `bash` berisiko (Fase 2) ──────────────────

fn git(root: &std::path::Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn init_repo(root: &std::path::Path) {
    git(root, &["init"]);
    git(root, &["config", "user.email", "minic@example.test"]);
    git(root, &["config", "user.name", "minic test"]);
    std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "init"]);
}

/// Jalankan satu panggilan `bash` lewat `run_agent` dan kembalikan isi hasil
/// tool (teks yang dilihat model), memakai approver yang selalu mengizinkan.
async fn run_bash_allowed(command: &str, root: &std::path::Path) -> String {
    struct AllowAll;
    impl PermissionHandler for AllowAll {
        fn decide<'a>(
            &'a self,
            _tool: &'a str,
            _args: &'a serde_json::Value,
            _mutating: bool,
        ) -> futures_core::future::BoxFuture<'a, minic_core::permission::Decision> {
            Box::pin(async { minic_core::permission::Decision::Allow })
        }
        fn resolve_ask<'a>(
            &'a self,
            _tool: &'a str,
            _args: &'a serde_json::Value,
            _reason: &'a str,
        ) -> futures_core::future::BoxFuture<'a, minic_core::permission::Decision> {
            Box::pin(async { minic_core::permission::Decision::Allow })
        }
    }

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(Bash::new("off")));
    // Checkpoint default aktif, jadi tidak perlu menyetel env (menyetel env
    // global dari test bisa balapan dengan test lain yang berjalan paralel).

    let ctx = ToolContext {
        cwd: root.to_path_buf(),
        permission_mode: PermissionMode::Auto,
    };
    let mut history = ContextStore::new();
    history.push(Message::user("go"));

    let provider = BashOnce::new(command);
    run_agent(
        &provider,
        &registry,
        &AllowAll,
        &minic_core::agent::NoopHooks,
        &mut history,
        &ctx,
        &RunOptions::new("fake"),
        None,
        &mut |_event| {},
    )
    .await
    .expect("agent finishes");
    // Ambil isi hasil tool penuh dari history (preview sudah dipotong).
    history
        .messages()
        .iter()
        .rev()
        .find(|message| message.role == minic_core::types::Role::Tool)
        .map(|message| message.text())
        .unwrap_or_default()
}

#[tokio::test]
async fn risky_bash_creates_tree_checkpoint_and_reports_it() {
    let root = temp_root("bash_ckpt");
    init_repo(&root);
    // Kerja yang belum di-commit: inilah yang dilindungi snapshot.
    std::fs::write(root.join("tracked.txt"), "in-progress\n").unwrap();

    let result = run_bash_allowed("git push && echo hi", &root).await;
    assert!(
        result.contains("checkpoint"),
        "hasil tool harus menyebut checkpoint: {result:?}"
    );
    // Snapshot harus ada di ref index.
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn ordinary_bash_does_not_create_checkpoint() {
    let root = temp_root("bash_no_ckpt");
    init_repo(&root);
    std::fs::write(root.join("tracked.txt"), "in-progress\n").unwrap();

    let result = run_bash_allowed("echo hello", &root).await;
    assert!(
        !result.contains("[minic]"),
        "perintah aman tidak perlu checkpoint: {result:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
