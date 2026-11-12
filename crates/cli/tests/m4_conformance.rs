//! M4: conformance (event JSONL deterministik) + property test primitif.

use std::path::PathBuf;

use minic::policy::SimplePolicy;
use minic::provider::fake::FakeProvider;
use minic::provider::sse::SseDecoder;
use minic::render::json;
use minic::tools::edit::apply_edit;
use minic::tools::read_file::ReadFile;
use minic_core::agent::{RunOptions, run_agent};
use minic_core::events::AgentEvent;
use minic_core::history::ContextStore;
use minic_core::permission::PermissionMode;
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::Message;
use proptest::prelude::*;
use serde_json::{Value, json};

// ── Conformance: urutan event deterministik dari provider fake ──

#[tokio::test]
async fn fake_run_event_stream_is_stable() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFile));

    let policy = SimplePolicy::new(root.clone(), PermissionMode::Auto);
    let ctx = ToolContext {
        cwd: root,
        permission_mode: PermissionMode::Auto,
    };

    let mut history = ContextStore::new();
    history.push(Message::user("inspect"));

    let provider = FakeProvider::default();
    let options = RunOptions::new("fake");

    let mut events: Vec<Value> = Vec::new();
    {
        let mut sink = |event: AgentEvent| events.push(json::event_value(&event));
        run_agent(
            &provider,
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
    }

    let expected = vec![
        json!({"v":1,"event":"turn_started","turn":1}),
        json!({"v":1,"event":"text_delta","text":"Let me look at the project.\n"}),
        json!({"v":1,"event":"tool_call_started","id":"call_demo_0","name":"read_file"}),
        json!({"v":1,"event":"tool_decided","id":"call_demo_0","name":"read_file","decision":"allow","reason":""}),
        json!({"v":1,"event":"tool_call_finished","id":"call_demo_0","name":"read_file","is_error":false}),
        json!({"v":1,"event":"turn_finished","turn":1}),
        json!({"v":1,"event":"turn_started","turn":2}),
        json!({"v":1,"event":"text_delta","text":"Done. (fake provider; 3 messages in context)\n"}),
        json!({"v":1,"event":"turn_finished","turn":2}),
        json!({"v":1,"event":"agent_finished","steps":2}),
    ];

    // Bandingkan per-nilai (urutan kunci JSON tidak relevan).
    assert_eq!(events.len(), expected.len(), "jumlah event: {events:#?}");
    for (index, (got, want)) in events.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "event #{index}");
    }
}

#[test]
fn json_line_has_version_and_event() {
    let line = json::line(&AgentEvent::TurnStarted { turn: 3 });
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["v"], json!(1));
    assert_eq!(value["event"], json!("turn_started"));
    assert_eq!(value["turn"], json!(3));
}

// ── Property: SSE decoder tidak bergantung pada titik potong chunk ──

fn sse_stream(payloads: &[String]) -> Vec<u8> {
    let mut text = String::new();
    for payload in payloads {
        text.push_str("data: ");
        text.push_str(payload);
        text.push_str("\n\n");
    }
    text.into_bytes()
}

proptest! {
    #[test]
    fn sse_decoder_is_split_invariant(
        payloads in proptest::collection::vec("[ -~]{0,20}", 0..6),
        cuts in proptest::collection::vec(0usize..48, 0..12),
    ) {
        let bytes = sse_stream(&payloads);

        let mut whole = SseDecoder::new();
        let complete = whole.push(&bytes);
        prop_assert_eq!(&complete, &payloads);

        let mut split = SseDecoder::new();
        let mut out: Vec<String> = Vec::new();
        let mut points: Vec<usize> = cuts.into_iter().filter(|c| *c <= bytes.len()).collect();
        points.sort_unstable();
        points.dedup();
        let mut prev = 0usize;
        for point in points.iter().copied().chain(std::iter::once(bytes.len())) {
            if point < prev {
                continue;
            }
            out.extend(split.push(&bytes[prev..point]));
            prev = point;
        }
        prop_assert_eq!(out, complete);
    }

    #[test]
    fn edit_replaces_unique_occurrence(
        prefix in ".*",
        marker in "[A-Za-z]{1,6}",
        suffix in ".*",
        replacement in "[A-Za-z]{0,6}",
    ) {
        let content = format!("{prefix}{marker}{suffix}");
        prop_assume!(content.matches(&marker).count() == 1);
        prop_assume!(marker != replacement);
        let (out, count) = apply_edit(&content, &marker, &replacement, false).unwrap();
        prop_assert_eq!(count, 1);
        prop_assert_eq!(out, format!("{prefix}{replacement}{suffix}"));
    }

    #[test]
    fn edit_reports_ambiguity(text in "[a-z]{1,3}") {
        // "x x x": tiga kemunculan tanpa replace_all harus error.
        let content = format!("{text} {text} {text}");
        prop_assume!(content.matches(&text).count() > 1);
        let result = apply_edit(&content, &text, "Z", false);
        prop_assert!(result.is_err());
    }
}
