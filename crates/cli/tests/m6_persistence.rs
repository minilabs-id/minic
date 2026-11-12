//! M6-j: persistensi streaming + resume.
//!
//! Regresi untuk bug nyata: history hanya disimpan setelah `run_agent`
//! selesai, sehingga provider yang gagal di tengah (atau proses yang mati)
//! meninggalkan sesi kosong. Sekarang kernel memanggil `HistoryPersistence`
//! di akhir **setiap** turn, dan host menyimpan snapshot saat itu juga.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::policy::SimplePolicy;
use minic::store::{SessionPersistence, SessionStore};
use minic::tools::write_file::WriteFile;
use minic_core::agent::{HistoryPersistence, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::PermissionMode;
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::{Message, Role};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_persist_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

/// Provider: turn 1 memanggil `write_file`, turn 2 menjawab teks.
struct OneWriteProvider;

impl ModelProvider for OneWriteProvider {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let already_ran = req.messages.iter().any(|m| m.role == Role::Tool);
        if already_ran {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta("done".to_string())));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        } else {
            let _ = tx.try_send(Ok(ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_w".to_string()),
                name: Some("write_file".to_string()),
                args_fragment: "{\"path\":\"x.txt\",\"content\":\"hi\"}".to_string(),
            }));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::ToolCalls)));
        }
        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

/// Merekam setiap snapshot yang dikirim kernel, tanpa I/O.
struct Recorder {
    snapshots: Mutex<Vec<Vec<Message>>>,
}

impl HistoryPersistence for Recorder {
    fn save(&self, messages: &[Message]) {
        self.snapshots.lock().unwrap().push(messages.to_vec());
    }
}

#[tokio::test]
async fn kernel_persists_at_every_turn_boundary() {
    let root = temp_root("turn");
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteFile));

    let policy = SimplePolicy::new(root.clone(), PermissionMode::Auto);
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };

    let mut history = ContextStore::new();
    history.push(Message::user("write x.txt"));

    let recorder = Recorder {
        snapshots: Mutex::new(Vec::new()),
    };
    let mut sink = |_event| {};
    run_agent(
        &OneWriteProvider,
        &registry,
        &policy,
        &minic_core::agent::NoopHooks,
        &mut history,
        &ctx,
        &RunOptions::new("fake"),
        Some(&recorder),
        &mut sink,
    )
    .await
    .unwrap();

    let snapshots = recorder.snapshots.lock().unwrap();
    // Turn 1 (tool call) + turn 2 (jawaban final) = dua snapshot, bukan satu.
    assert_eq!(snapshots.len(), 2, "save harus terjadi tiap akhir turn");
    // Snapshot pertama: user + assistant(tool_calls) + tool result.
    assert_eq!(snapshots[0].len(), 3);
    assert_eq!(snapshots[0][2].role, Role::Tool);
    // Snapshot terakhir identik dengan history final.
    assert_eq!(snapshots.last().unwrap().as_slice(), history.messages());
}

#[test]
fn session_persistence_survives_and_resumes() {
    let root = temp_root("resume");
    let store = SessionStore::open(&root).unwrap();
    let persistence = SessionPersistence::new(store, "s-resume");
    persistence.save(&[Message::user("hello"), Message::assistant("world")]);

    // Simulasi proses baru: buka store yang sama dan baca kembali.
    let store = SessionStore::open(&root).unwrap();
    let loaded = store.load_messages("s-resume").unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].text(), "hello");
    assert_eq!(loaded[1].text(), "world");
}
