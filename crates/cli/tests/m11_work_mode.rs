//! Mode kerja: diskusi (read-only) dan eksekusi (boleh mengubah).
//!
//! Yang dijaga di sini bukan tampilan, melainkan **penegakan**: mode diskusi
//! harus membuat tool yang mengubah ditolak oleh policy. Mode yang hanya
//! diatur lewat teks prompt bisa bocor — agent lain dilaporkan tetap menulis
//! berkas saat layar menampilkan mode rencana.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::policy::SimplePolicy;
use minic::tools::write_file::WriteFile;
use minic::tui::app::WorkMode;
use minic_core::agent::{RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::{Decision, PermissionHandler, PermissionMode};
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::{Message, Role};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_m11_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

/// Mode kerja memetakan ke mode izin yang menegakkannya.
#[test]
fn work_modes_map_to_enforcing_permission_modes() {
    assert_eq!(WorkMode::Discuss.to_permission(), PermissionMode::ReadOnly);
    assert_eq!(WorkMode::Execute.to_permission(), PermissionMode::Auto);
    assert_eq!(WorkMode::Discuss.label(), "discuss");
    assert_eq!(WorkMode::Execute.label(), "execute");
}

/// Mode diskusi menolak tool yang mengubah — di lapisan policy, bukan prompt.
#[tokio::test]
async fn discuss_mode_denies_mutating_tools() {
    let root = temp_root("deny");
    let policy = SimplePolicy::new(root.clone(), WorkMode::Discuss.to_permission());
    let decision = policy
        .decide("write_file", &serde_json::json!({"path": "a.txt"}), true)
        .await;
    match decision {
        Decision::Deny(reason) => assert!(
            reason.contains("readonly"),
            "alasan penolakan harus menyebut readonly: {reason}"
        ),
        other => panic!("mode diskusi harus menolak mutating tool, dapat {other:?}"),
    }
}

/// Mode diskusi tetap mengizinkan tool baca.
#[tokio::test]
async fn discuss_mode_allows_read_only_tools() {
    let root = temp_root("allow_read");
    let policy = SimplePolicy::new(root.clone(), WorkMode::Discuss.to_permission());
    let decision = policy
        .decide("read_file", &serde_json::json!({"path": "a.txt"}), false)
        .await;
    assert_eq!(
        decision,
        Decision::Allow,
        "membaca harus tetap boleh di mode diskusi"
    );
}

/// Beralih mode saat sesi berjalan langsung mengubah keputusan berikutnya.
#[tokio::test]
async fn switching_mode_takes_effect_without_restart() {
    let root = temp_root("switch");
    let policy = SimplePolicy::new(root.clone(), WorkMode::Execute.to_permission());
    let args = serde_json::json!({"path": "a.txt"});

    assert_eq!(
        policy.decide("write_file", &args, true).await,
        Decision::Allow,
        "mode eksekusi harus mengizinkan tulis"
    );

    // Sama seperti `/discuss`: hanya mode yang diganti, objek policy sama.
    policy.set_mode(WorkMode::Discuss.to_permission());
    assert!(
        matches!(
            policy.decide("write_file", &args, true).await,
            Decision::Deny(_)
        ),
        "setelah /discuss, tulis harus ditolak pada policy yang sama"
    );

    // Dan kembali lagi.
    policy.set_mode(WorkMode::Execute.to_permission());
    assert_eq!(
        policy.decide("write_file", &args, true).await,
        Decision::Allow,
        "setelah /execute, tulis harus diizinkan lagi"
    );
}

/// Mode dibaca ulang setiap keputusan: policy dipakai bersama antar turn, jadi
/// mode yang dipegang di awal tidak boleh "menempel" di dalam struct.
#[tokio::test]
async fn mode_is_read_per_decision_not_cached() {
    let root = temp_root("percall");
    let policy = Arc::new(SimplePolicy::new(
        root.clone(),
        WorkMode::Discuss.to_permission(),
    ));
    let args = serde_json::json!({"path": "a.txt"});

    for expected_denied in [true, false, true] {
        if !expected_denied {
            policy.set_mode(PermissionMode::Auto);
        }
        let decision = policy.decide("write_file", &args, true).await;
        assert_eq!(
            matches!(decision, Decision::Deny(_)),
            expected_denied,
            "keputusan harus mengikuti mode saat ini"
        );
        policy.set_mode(WorkMode::Discuss.to_permission());
    }
}

/// Nilai mode yang tidak dikenal dibaca sebagai `Ask` (paling aman yang masih
/// bisa bekerja), bukan `Auto`.
#[test]
fn unknown_mode_byte_falls_back_to_ask() {
    assert_eq!(PermissionMode::from_u8(0), PermissionMode::Auto);
    assert_eq!(PermissionMode::from_u8(1), PermissionMode::Ask);
    assert_eq!(PermissionMode::from_u8(2), PermissionMode::ReadOnly);
    assert_eq!(PermissionMode::from_u8(200), PermissionMode::Ask);
}

/// Round-trip `to_u8`/`from_u8` untuk semua mode.
#[test]
fn mode_bytes_round_trip() {
    for mode in [
        PermissionMode::Auto,
        PermissionMode::Ask,
        PermissionMode::ReadOnly,
    ] {
        assert_eq!(PermissionMode::from_u8(mode.to_u8()), mode);
    }
}

/// Provider yang meminta `write_file` pada putaran pertama, lalu selesai.
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

async fn run_write_in_mode(root: &std::path::Path, mode: PermissionMode) -> ContextStore {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteFile));

    let policy = SimplePolicy::new(root.to_path_buf(), mode);
    let ctx = ToolContext {
        cwd: root.to_path_buf(),
    };
    let mut history = ContextStore::new();
    history.push(Message::user("write x.txt"));
    let options = RunOptions::new("fake");
    let mut sink = |_event| {};
    run_agent(
        &OneWriteProvider,
        &registry,
        &policy,
        &minic_core::agent::NoopHooks,
        &mut history,
        &ctx,
        &options,
        None,
        &mut sink,
    )
    .await
    .unwrap();
    history
}

/// End-to-end: mode diskusi menolak tulis dan **tidak menyentuh disk**.
#[tokio::test]
async fn discuss_mode_blocks_a_real_write_end_to_end() {
    let root = temp_root("e2e_discuss");
    let history = run_write_in_mode(&root, WorkMode::Discuss.to_permission()).await;

    let tool_message = history
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("ada pesan tool");
    assert!(tool_message.is_error, "hasil tool harus ditandai error");
    assert!(
        tool_message.text().contains("permission denied"),
        "pesan harus menyebut izin ditolak: {}",
        tool_message.text()
    );
    assert!(
        !root.join("x.txt").exists(),
        "mode diskusi tidak boleh membuat berkas"
    );
}

/// Pembanding: mode eksekusi memang menulis, jadi test di atas tidak lulus
/// hanya karena berkasnya tidak pernah bisa dibuat.
#[tokio::test]
async fn execute_mode_writes_end_to_end() {
    let root = temp_root("e2e_execute");
    run_write_in_mode(&root, WorkMode::Execute.to_permission()).await;
    assert!(
        root.join("x.txt").exists(),
        "mode eksekusi harus benar-benar menulis"
    );
}
