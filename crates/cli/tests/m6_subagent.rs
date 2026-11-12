//! M6-d: sub-agent pool, batas maksimum 10, tanpa nesting.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::model_session::ModelSession;
use minic::provider::fake::FakeProvider;
use minic::subagent::{MAX_SUBAGENTS, SubAgentPool};
use minic::tools::delegate::DelegateTask;
use minic_core::permission::PermissionMode;
use minic_core::provider::ModelProvider;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_sub_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn pool(root: &std::path::Path) -> SubAgentPool {
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::default());
    let session = Arc::new(ModelSession::new(Some(provider), "fake", 128_000));
    SubAgentPool::new(session, None, 10, root.to_path_buf())
}

#[test]
fn capacity_is_ten() {
    assert_eq!(MAX_SUBAGENTS, 10);
    let root = temp_root("cap");
    assert_eq!(pool(&root).capacity(), 10);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn runs_ten_parallel_subagents() {
    let root = temp_root("parallel");
    let tasks: Vec<String> = (1..=10).map(|i| format!("task {i}")).collect();
    let results = pool(&root).run_many(tasks).await;
    assert_eq!(results.len(), 10);
    for (index, result) in results.iter().enumerate() {
        let text = result
            .as_ref()
            .unwrap_or_else(|e| panic!("task {index}: {e}"));
        assert!(!text.trim().is_empty(), "task {index} kosong");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn delegate_rejects_more_than_ten() {
    let root = temp_root("reject");
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };
    let tasks: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
    let result = DelegateTask::new(Arc::new(pool(&root)))
        .execute(json!({ "tasks": tasks }), &ctx)
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(
        result.content.contains("maximum is 10"),
        "isi: {}",
        result.content
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn delegate_accepts_ten() {
    let root = temp_root("accept");
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };
    let tasks: Vec<String> = (1..=10).map(|i| format!("task {i}")).collect();
    let result = DelegateTask::new(Arc::new(pool(&root)))
        .execute(json!({ "tasks": tasks }), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("### sub-agent 10 (ok)"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn empty_tasks_is_rejected() {
    let root = temp_root("empty");
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };
    let result = DelegateTask::new(Arc::new(pool(&root)))
        .execute(json!({ "tasks": [] }), &ctx)
        .await
        .unwrap();
    assert!(result.is_error);
    let _ = std::fs::remove_dir_all(&root);
}
