//! M6-h: hooks pre/post tool.
//!
//! Pre-hook yang gagal harus **memblokir** tool (fail-closed); post-hook
//! dipanggil dengan `exit_code` hasil tool. Skrip membaca payload JSON dari
//! `MINIC_HOOK_PAYLOAD` (variabel lingkungan), jadi test menulis `.cmd`
//! (Windows) / `.sh` (Unix).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_core::future::BoxFuture;
use minic::hooks_impl::FileHooks;
use minic::tools::write_file::WriteFile;
use minic_core::agent::{AgentHooks, RunOptions, run_agent};
use minic_core::error::AgentError;
use minic_core::history::ContextStore;
use minic_core::permission::{Decision, PermissionMode};
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::{Message, Role};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_hooks_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

/// Tulis skrip hook yang: mencatat payload env ke `out`, lalu exit `code`.
fn write_hook(root: &Path, name: &str, body: &str) {
    let dir = root.join(".minic").join("hooks");
    std::fs::create_dir_all(&dir).unwrap();
    let (file, content) = if cfg!(windows) {
        (format!("{name}.cmd"), format!("@echo off\r\n{body}\r\n"))
    } else {
        (format!("{name}.sh"), format!("#!/bin/sh\n{body}\n"))
    };
    let path = dir.join(file);
    std::fs::write(&path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
}

/// Skrip: tulis payload env ke berkas `out_name`, lalu exit `exit_code`.
fn capture_payload_to(out_name: &str, exit_code: i32) -> String {
    if cfg!(windows) {
        format!("> {out_name} echo %MINIC_HOOK_PAYLOAD%\r\nexit /b {exit_code}")
    } else {
        format!("echo \"$MINIC_HOOK_PAYLOAD\" > {out_name}\nexit {exit_code}")
    }
}

#[test]
fn pre_hook_failure_blocks_tool() {
    let root = temp_root("prefail");
    write_hook(&root, "pre_write_file", &capture_payload_to("pre.json", 1));

    let hooks = FileHooks::new(root.clone());
    let args = json!({ "path": "x.txt", "content": "hi" });
    let error = hooks.pre_tool("write_file", &args, &root).unwrap_err();
    assert!(error.contains("pre-hook for write_file failed"), "{error}");

    // Payload JSON harus sampai ke skrip lewat variabel lingkungan.
    let captured = std::fs::read_to_string(root.join("pre.json")).unwrap_or_default();
    assert!(captured.contains("\"tool\""), "payload stdin: {captured}");
    assert!(captured.contains("write_file"), "payload stdin: {captured}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn absent_hook_is_noop() {
    let root = temp_root("noop");
    let hooks = FileHooks::new(root.clone());
    assert!(hooks.pre_tool("write_file", &json!({}), &root).is_ok());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn post_hook_receives_exit_code() {
    let root = temp_root("post");
    write_hook(
        &root,
        "post_write_file",
        &capture_payload_to("post.json", 0),
    );

    let hooks = FileHooks::new(root.clone());
    let warning = hooks.post_tool("write_file", &json!({ "path": "x.txt" }), &root, 1);
    assert!(warning.is_none(), "hook sukses tidak boleh memperingatkan");

    let captured = std::fs::read_to_string(root.join("post.json")).unwrap_or_default();
    assert!(captured.contains("\"exit_code\":1"), "payload: {captured}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Post-hook yang gagal harus **mengembalikan** peringatan, bukan mencetaknya
/// ke stderr — di TUI (alternate screen) stderr tidak terlihat, jadi kegagalan
/// hook dulu hilang tanpa jejak.
#[test]
fn failing_post_hook_is_reported_to_the_caller() {
    let root = temp_root("post_fail");
    write_hook(&root, "post_write_file", "exit /b 7");

    let hooks = FileHooks::new(root.clone());
    let warning = hooks.post_tool("write_file", &json!({ "path": "x.txt" }), &root, 0);

    let warning = warning.expect("kegagalan post-hook harus dilaporkan");
    assert!(warning.contains("write_file"), "warning: {warning}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn absent_post_hook_is_silent() {
    let root = temp_root("post_none");
    let hooks = FileHooks::new(root.clone());
    assert!(
        hooks
            .post_tool("write_file", &json!({}), &root, 0)
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Provider: panggil `write_file` sekali, lalu selesai.
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

#[tokio::test]
async fn denying_pre_hook_prevents_disk_write() {
    let root = temp_root("e2e");
    write_hook(&root, "pre_write_file", &capture_payload_to("pre.json", 1));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteFile));
    let hooks = FileHooks::new(root.clone());
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Auto,
    };

    let mut history = ContextStore::new();
    history.push(Message::user("write x.txt"));
    let options = RunOptions::new("fake");
    let mut sink = |_event| {};
    run_agent(
        &OneWriteProvider,
        &registry,
        &AskThenAllow,
        &hooks,
        &mut history,
        &ctx,
        &options,
        None,
        &mut sink,
    )
    .await
    .unwrap();

    let tool_message = history
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("ada pesan tool");
    assert!(tool_message.is_error);
    assert!(
        tool_message.text().contains("hook pre denied"),
        "{}",
        tool_message.text()
    );
    assert!(
        !root.join("x.txt").exists(),
        "pre-hook gagal harus mencegah tulisan"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hooks_work_from_path_with_spaces() {
    // Regresi: `cmd.exe /C` harus menerima path workspace ber-spasi.
    let base = temp_root("spaced base");
    let root = base.join("dir with spaces");
    std::fs::create_dir_all(&root).unwrap();
    write_hook(&root, "pre_write_file", &capture_payload_to("pre.json", 0));

    let hooks = FileHooks::new(root.clone());
    let args = json!({ "path": "x.txt", "content": "hi" });
    assert!(hooks.pre_tool("write_file", &args, &root).is_ok());
    let captured = std::fs::read_to_string(root.join("pre.json")).unwrap_or_default();
    assert!(captured.contains("write_file"), "payload: {captured}");

    let _ = std::fs::remove_dir_all(&base);
}

/// Policy yang menandai mutasi sebagai `Ask` lalu menyetujuinya di
/// `resolve_ask` — mensimulasikan mode `ask` dengan user menekan y.
struct AskThenAllow;

impl minic_core::permission::PermissionHandler for AskThenAllow {
    fn decide<'a>(
        &'a self,
        _tool: &'a str,
        _args: &'a serde_json::Value,
        mutating: bool,
    ) -> BoxFuture<'a, Decision> {
        let decision = if mutating {
            Decision::Ask("user must approve".to_string())
        } else {
            Decision::Allow
        };
        Box::pin(async move { decision })
    }

    fn resolve_ask<'a>(
        &'a self,
        _tool: &'a str,
        _args: &'a serde_json::Value,
        _reason: &'a str,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async move { Decision::Allow })
    }
}

#[tokio::test]
async fn denying_pre_hook_blocks_even_after_ask_approval() {
    // Regresi: pre-hook harus dijalankan dengan keputusan FINAL. Sebelum
    // audit, pre-hook hanya dicek pada cabang `Allow` langsung — di mode
    // ask yang disetujui, tool dieksekusi tanpa pre-hook (bypass).
    let root = temp_root("ask");
    write_hook(&root, "pre_write_file", &capture_payload_to("pre.json", 1));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteFile));
    let hooks = FileHooks::new(root.clone());
    let ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: PermissionMode::Ask,
    };

    let mut history = ContextStore::new();
    history.push(Message::user("write x.txt"));
    let options = RunOptions::new("fake");
    let mut sink = |_event| {};
    run_agent(
        &OneWriteProvider,
        &registry,
        &AskThenAllow,
        &hooks,
        &mut history,
        &ctx,
        &options,
        None,
        &mut sink,
    )
    .await
    .unwrap();

    let tool_message = history
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("ada pesan tool");
    assert!(tool_message.is_error);
    assert!(
        tool_message.text().contains("hook pre denied"),
        "{}",
        tool_message.text()
    );
    assert!(
        !root.join("x.txt").exists(),
        "pre-hook deny harus memblokir tool meski ask disetujui"
    );

    let _ = std::fs::remove_dir_all(&root);
}
