//! M6-c: klien MCP end-to-end dengan server palsu (bin `fake-mcp-server`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::mcp::McpManager;
use minic::tools::mcp::{McpCall, McpList};
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_mcp_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn write_mcp_config(root: &std::path::Path) {
    let server = env!("CARGO_BIN_EXE_fake-mcp-server");
    let cfg = json!({ "servers": { "demo": { "command": server, "args": [] } } });
    std::fs::create_dir_all(root.join(".minic")).unwrap();
    std::fs::write(
        root.join(".minic/mcp.json"),
        serde_json::to_string(&cfg).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn mcp_connect_list_call_roundtrip() {
    let root = temp_root("roundtrip");
    write_mcp_config(&root);

    let manager = Arc::new(McpManager::connect(&root).await);
    assert!(
        manager.errors().is_empty(),
        "errors: {:?}",
        manager.errors()
    );
    assert_eq!(manager.names(), vec!["demo".to_string()]);

    let ctx = ToolContext { cwd: root.clone() };

    let listed = McpList::new(manager.clone())
        .execute(json!({}), &ctx)
        .await
        .unwrap();
    assert!(listed.content.contains("echo"), "isi: {}", listed.content);

    let called = McpCall::new(manager.clone())
        .execute(
            json!({ "server": "demo", "tool": "echo", "arguments": { "text": "hi" } }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!called.is_error);
    assert_eq!(called.content, "echo:hi");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn unknown_server_reports_available() {
    let root = temp_root("unknown");
    write_mcp_config(&root);
    let manager = Arc::new(McpManager::connect(&root).await);
    let ctx = ToolContext { cwd: root.clone() };

    let result = McpCall::new(manager)
        .execute(
            json!({ "server": "nope", "tool": "echo", "arguments": {} }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("unknown MCP server"));
    assert!(result.content.contains("demo"));

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn no_config_is_empty() {
    let root = temp_root("empty");
    let manager = McpManager::connect(&root).await;
    assert!(manager.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
