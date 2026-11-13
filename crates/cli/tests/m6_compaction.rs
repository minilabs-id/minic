//! M6-e: kompaksi konteks.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minic_core::agent::{RunOptions, run_agent};
use minic_core::compaction::mechanical_compact;
use minic_core::error::AgentError;
use minic_core::events::AgentEvent;
use minic_core::history::ContextStore;
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::{Message, Role, ToolCall, ToolResult};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_compact_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

#[test]
fn folds_old_and_keeps_recent() {
    let mut messages = Vec::new();
    for i in 0..20 {
        if i % 2 == 0 {
            messages.push(Message::user(format!("u{i}")));
        } else {
            messages.push(Message::assistant(format!("a{i}")));
        }
    }
    let original_tail: Vec<Message> = messages[messages.len() - 4..].to_vec();

    let (compacted, elided) = mechanical_compact(&messages, 4);
    assert!(elided > 0, "harus ada yang dilipat");
    assert!(compacted.len() < messages.len());
    assert_eq!(&compacted[compacted.len() - 4..], original_tail.as_slice());
}

#[test]
fn preserves_leading_system_messages() {
    let mut messages = vec![Message::system("SYS")];
    for i in 0..12 {
        messages.push(Message::user(format!("u{i}")));
    }
    let (compacted, elided) = mechanical_compact(&messages, 3);
    assert!(elided > 0);
    assert_eq!(compacted[0].role, Role::System);
    assert_eq!(compacted[0].text(), "SYS");
}

#[test]
fn never_starts_kept_window_with_orphan_tool() {
    let calls = vec![ToolCall {
        id: "c1".to_string(),
        name: "x".to_string(),
        arguments: json!({}),
    }];
    let messages = vec![
        Message::user("intro"),
        Message::assistant_with("", calls.clone()),
        Message::tool_result(ToolResult::ok("c1", "x", "r1")),
        Message::tool_result(ToolResult::ok("c1", "x", "r2")),
        Message::user("next"),
        Message::assistant_with("", calls),
        Message::tool_result(ToolResult::ok("c1", "x", "r3")),
        Message::user("q"),
        Message::assistant("a"),
    ];

    let (compacted, elided) = mechanical_compact(&messages, 3);
    assert!(elided > 0);
    let first_body = compacted.iter().find(|m| m.role != Role::System);
    assert!(
        !matches!(first_body.map(|m| m.role), Some(Role::Tool)),
        "jendela tidak boleh mulai dengan Tool yatim"
    );
}

/// Provider yang langsung membalas teks lalu selesai.
struct StopProvider;

impl ModelProvider for StopProvider {
    fn stream(&self, _req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let _ = tx.try_send(Ok(ProviderEvent::TextDelta("ok".to_string())));
        let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

#[tokio::test]
async fn agent_emits_context_compacted_when_over_budget() {
    let root = temp_root("e2e");
    let ctx = ToolContext { cwd: root.clone() };
    let registry = ToolRegistry::new();

    let mut history = ContextStore::new();
    for i in 0..50 {
        if i % 2 == 0 {
            history.push(Message::user(format!("u{i}-{}", "x".repeat(100))));
        } else {
            history.push(Message::assistant(format!("a{i}-{}", "y".repeat(100))));
        }
    }
    history.push(Message::user("final"));
    let before = history.len();

    let mut options = RunOptions::new("fake");
    options.context_window_tokens = Some(50); // budget sangat kecil → paksa kompaksi
    options.keep_recent = 4;

    let provider = StopProvider;
    let mut events: Vec<AgentEvent> = Vec::new();
    let mut sink = |event: AgentEvent| events.push(event);
    run_agent(
        &provider,
        &registry,
        &minic_core::permission::AllowAll,
        &minic_core::agent::NoopHooks,
        &mut history,
        &ctx,
        &options,
        None,
        &mut sink,
    )
    .await
    .unwrap();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ContextCompacted { elided } if *elided > 0)),
        "harus memancarkan ContextCompacted"
    );
    assert!(history.len() < before, "history harus mengecil");

    let _ = std::fs::remove_dir_all(&root);
}
