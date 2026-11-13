//! M1 integration: tool berkas, edit, grep, glob, bash, dan jail.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::policy::SimplePolicy;
use minic::tools::bash::Bash;
use minic::tools::edit::Edit;
use minic::tools::glob::Glob;
use minic::tools::grep::Grep;
use minic::tools::read_file::ReadFile;
use minic::tools::write_file::WriteFile;
use minic_core::permission::{Decision, PermissionHandler, PermissionMode};
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("minic_m1_{tag}_{nanos}"));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        Self { root }
    }

    fn ctx(&self) -> ToolContext {
        ToolContext {
            cwd: self.root.clone(),
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text_of(result: &minic_core::types::ToolResult) -> &str {
    &result.content
}

#[tokio::test]
async fn write_edit_read_roundtrip() {
    let ws = Workspace::new("rw");
    let ctx = ws.ctx();

    let written = WriteFile
        .execute(
            json!({ "path": "src/lib.rs", "content": "pub fn a() -> i32 { 1 }\n" }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!written.is_error);

    let edited = Edit
        .execute(
            json!({ "path": "src/lib.rs", "old_string": "1", "new_string": "2" }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(text_of(&edited).contains("replaced 1"));

    let read = ReadFile
        .execute(json!({ "path": "src/lib.rs" }), &ctx)
        .await
        .unwrap();
    assert!(text_of(&read).contains("{ 2 }"));
}

#[tokio::test]
async fn edit_is_ambiguous_without_replace_all() {
    let ws = Workspace::new("amb");
    let ctx = ws.ctx();
    WriteFile
        .execute(json!({ "path": "a.txt", "content": "x x x" }), &ctx)
        .await
        .unwrap();
    let result = Edit
        .execute(
            json!({ "path": "a.txt", "old_string": "x", "new_string": "y" }),
            &ctx,
        )
        .await;
    assert!(result.is_err(), "edit ambigu harus error");
}

#[tokio::test]
async fn grep_and_glob_find_things() {
    let ws = Workspace::new("search");
    let ctx = ws.ctx();
    WriteFile
        .execute(
            json!({ "path": "src/main.rs", "content": "fn answer() -> u32 { 42 }\n" }),
            &ctx,
        )
        .await
        .unwrap();
    WriteFile
        .execute(json!({ "path": "README.md", "content": "# hi\n" }), &ctx)
        .await
        .unwrap();

    let grepped = Grep
        .execute(json!({ "pattern": "answer" }), &ctx)
        .await
        .unwrap();
    assert!(text_of(&grepped).contains("src/main.rs:1:"));

    let globbed = Glob
        .execute(json!({ "pattern": "**/*.rs" }), &ctx)
        .await
        .unwrap();
    assert!(text_of(&globbed).contains("src/main.rs"));
    assert!(!text_of(&globbed).contains("README.md"));
}

#[tokio::test]
async fn jail_blocks_escape_and_sensitive() {
    let ws = Workspace::new("jail");
    let ctx = ws.ctx();
    assert!(
        ReadFile
            .execute(json!({ "path": "../escape.txt" }), &ctx)
            .await
            .is_err()
    );
    assert!(
        WriteFile
            .execute(json!({ "path": "id_rsa", "content": "secret" }), &ctx)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bash_captures_output() {
    let ws = Workspace::new("bash");
    let ctx = ws.ctx();
    let result = Bash::new("off")
        .execute(json!({ "command": "echo minic-ok" }), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(text_of(&result).contains("minic-ok"));
    assert!(text_of(&result).contains("exit code: 0"));
}

#[tokio::test]
async fn policy_readonly_denies_mutation_and_escape() {
    let ws = Workspace::new("policy");
    let policy = SimplePolicy::new(ws.root.clone(), PermissionMode::ReadOnly);

    let read = policy
        .decide("read_file", &json!({ "path": "a.txt" }), false)
        .await;
    assert_eq!(read, Decision::Allow);

    let write = policy
        .decide("write_file", &json!({ "path": "a.txt" }), true)
        .await;
    assert!(
        matches!(write, Decision::Deny(_)),
        "readonly harus menolak mutasi"
    );

    let escape = policy
        .decide("read_file", &json!({ "path": "../x" }), false)
        .await;
    assert!(matches!(escape, Decision::Deny(_)), "escape harus ditolak");
}

#[cfg(windows)]
#[tokio::test]
async fn bash_timeout_is_unknown() {
    let ws = Workspace::new("timeout");
    let ctx = ws.ctx();
    let result = Bash::new("off")
        .execute(
            json!({ "command": "ping -n 6 127.0.0.1 >nul", "timeout_ms": 400 }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(result.is_error, "timeout harus is_error");
    assert!(text_of(&result).contains("outcome UNKNOWN"));
}

#[tokio::test]
async fn grep_offset_pages_through_results() {
    let ws = Workspace::new("grep_offset");
    let ctx = ws.ctx();
    let body: String = (1..=20).map(|i| format!("needle {i}\n")).collect();
    WriteFile
        .execute(json!({ "path": "hits.txt", "content": body }), &ctx)
        .await
        .unwrap();

    let first = Grep
        .execute(json!({ "pattern": "needle", "max_results": 5 }), &ctx)
        .await
        .unwrap();
    let first_lines: Vec<&str> = first
        .content
        .lines()
        .filter(|line| !line.starts_with("… ["))
        .collect();
    assert_eq!(
        first_lines.len(),
        5,
        "footer is not a match: {}",
        first.content
    );
    assert!(first.content.contains("needle 1"));
    assert!(!first.content.contains("needle 6"));
    assert!(
        first.content.contains("next page: offset=5"),
        "{}",
        first.content
    );

    let second = Grep
        .execute(
            json!({ "pattern": "needle", "max_results": 5, "offset": 5 }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(second.content.contains("needle 6"), "{}", second.content);
    assert!(!second.content.contains("needle 1:"), "{}", second.content);
}

#[tokio::test]
async fn read_file_offset_and_limit_are_honoured() {
    let ws = Workspace::new("read_page");
    let ctx = ws.ctx();
    let body: String = (1..=50).map(|i| format!("row {i}\n")).collect();
    WriteFile
        .execute(json!({ "path": "big.txt", "content": body }), &ctx)
        .await
        .unwrap();

    let page = ReadFile
        .execute(json!({ "path": "big.txt", "offset": 10, "limit": 3 }), &ctx)
        .await
        .unwrap();
    assert!(
        page.content.contains("lines 10-12 of 50"),
        "{}",
        page.content
    );
    assert!(page.content.contains("row 10"));
    assert!(page.content.contains("row 12"));
    assert!(!page.content.contains("row 9\n"));
    assert!(!page.content.contains("row 13"));

    // Tanpa offset/limit → perilaku lama: seluruh berkas, tanpa header.
    let whole = ReadFile
        .execute(json!({ "path": "big.txt" }), &ctx)
        .await
        .unwrap();
    assert!(!whole.content.contains("[minic] lines"));
    assert!(whole.content.contains("row 50"));
}
