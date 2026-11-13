//! M0 smoke test: fake provider memicu satu tool call, lalu selesai.

use std::path::PathBuf;

use minic::policy::SimplePolicy;
use minic::provider::fake::FakeProvider;
use minic::tools::read_file::ReadFile;
use minic_core::agent::{RunOptions, run_agent};
use minic_core::events::AgentEvent;
use minic_core::history::ContextStore;
use minic_core::permission::PermissionMode;
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::{Message, Role};

#[tokio::test]
async fn fake_provider_runs_one_tool_then_finishes() {
    // Crate root punya Cargo.toml → read_file("Cargo.toml") akan sukses.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFile));

    let policy = SimplePolicy::new(root.clone(), PermissionMode::Auto);
    let ctx = ToolContext { cwd: root };

    let mut history = ContextStore::new();
    history.push(Message::user("inspect the project"));

    let provider = FakeProvider::default();
    let options = RunOptions::new("fake");

    let mut events = 0usize;
    let outcome = {
        let mut on_event = |event: AgentEvent| {
            let _ = &event;
            events += 1;
        };
        run_agent(
            &provider,
            &registry,
            &policy,
            &minic_core::agent::NoopHooks,
            &mut history,
            &ctx,
            &options,
            None,
            &mut on_event,
        )
        .await
        .expect("agent run should succeed")
    };

    assert_eq!(outcome.tool_calls, 1, "harus tepat satu tool call");
    assert!(outcome.steps >= 2, "harus lewat minimal dua langkah");
    assert!(events > 0, "harus memancarkan event");

    // Tool result terpasang di history sebagai pesan Role::Tool yang sukses.
    let tool_message = history
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("ada pesan hasil tool");
    assert!(!tool_message.is_error, "read_file Cargo.toml harus sukses");
    assert!(tool_message.text().contains("[package]"));
}
