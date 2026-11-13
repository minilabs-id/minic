//! M6-g: LSP diagnostics end-to-end dengan server palsu (bin `fake-lsp-server`).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::tools::lsp::LspDiagnostics;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_lsp_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn write_lsp_config(root: &std::path::Path) {
    let server = env!("CARGO_BIN_EXE_fake-lsp-server");
    let cfg = json!({ "servers": { "rs": { "command": server, "args": [] } } });
    std::fs::create_dir_all(root.join(".minic")).unwrap();
    std::fs::write(
        root.join(".minic/lsp.json"),
        serde_json::to_string(&cfg).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn returns_diagnostics_from_server() {
    let root = temp_root("diag");
    write_lsp_config(&root);
    std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let ctx = ToolContext { cwd: root.clone() };
    let result = LspDiagnostics
        .execute(json!({ "path": "main.rs" }), &ctx)
        .await
        .unwrap();

    assert!(!result.is_error, "isi: {}", result.content);
    assert!(
        result.content.contains("fake diagnostic"),
        "isi: {}",
        result.content
    );
    assert!(result.content.contains("[error]"));

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn unconfigured_extension_reports_error() {
    let root = temp_root("noext");
    std::fs::write(root.join("main.py"), "print('x')\n").unwrap();
    let ctx = ToolContext { cwd: root.clone() };
    let result = LspDiagnostics
        .execute(json!({ "path": "main.py" }), &ctx)
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("no LSP server configured"));
    let _ = std::fs::remove_dir_all(&root);
}
