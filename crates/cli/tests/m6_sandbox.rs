//! M6-a: sandbox fail-closed.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::tools::bash::Bash;
use minic_core::permission::PermissionMode;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_m6_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

#[tokio::test]
async fn unknown_sandbox_value_fails_closed() {
    let root = temp_root("bogus");
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };

    let result = Bash::new("bogus")
        .execute(json!({ "command": "echo hi > marker.txt" }), &ctx)
        .await
        .unwrap();

    assert!(result.is_error, "sandbox tak dikenal harus fail-closed");
    assert!(result.content.contains("sandbox unavailable"));
    assert!(
        !root.join("marker.txt").exists(),
        "perintah tidak boleh dijalankan di host"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn off_runs_on_host() {
    let root = temp_root("off");
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };

    let result = Bash::new("off")
        .execute(json!({ "command": "echo hi > marker.txt" }), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(root.join("marker.txt").exists());

    let _ = std::fs::remove_dir_all(&root);
}
