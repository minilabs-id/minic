//! M5: state machine TUI (murni) — tanpa PTY.

use minic::tui::app::{Action, App, Key, Line, ToolState};
use minic_core::events::AgentEvent;

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(Key::Char(c));
    }
}

#[test]
fn typing_backspace_and_submit() {
    let mut app = App::new("mode=auto");
    type_str(&mut app, "hello");
    app.handle_key(Key::Char('!'));
    assert_eq!(app.input(), "hello!");

    app.handle_key(Key::Backspace);
    assert_eq!(app.input(), "hello");

    let action = app.handle_key(Key::Enter);
    assert_eq!(action, Action::Submit("hello".to_string()));
    assert_eq!(app.input(), "");
    assert!(
        app.transcript()
            .iter()
            .any(|line| matches!(line, Line::User(t) if t == "hello"))
    );
}

#[test]
fn empty_submit_is_ignored() {
    let mut app = App::new("s");
    assert_eq!(app.handle_key(Key::Enter), Action::None);
}

#[test]
fn cursor_editing_moves_by_char() {
    let mut app = App::new("s");
    type_str(&mut app, "ac");
    app.handle_key(Key::Left);
    app.handle_key(Key::Char('b'));
    assert_eq!(app.input(), "abc");
}

#[test]
fn agent_events_build_transcript() {
    let mut app = App::new("s");
    app.begin_turn();
    assert!(app.busy());

    app.on_agent_event(AgentEvent::TextDelta {
        text: "hi ".to_string(),
    });
    app.on_agent_event(AgentEvent::TextDelta {
        text: "there".to_string(),
    });
    assert_eq!(app.assistant_live(), "hi there");

    app.on_agent_event(AgentEvent::ToolCallStarted {
        id: "c1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({ "path": "src/main.rs" }),
    });
    // Buffer assistant di-flush ke transcript sebelum tool line.
    assert!(
        app.transcript()
            .iter()
            .any(|line| matches!(line, Line::Assistant(t) if t == "hi there"))
    );
    assert!(app.transcript().iter().any(|line| matches!(
        line,
        Line::Tool { name, summary, state: ToolState::Running, .. }
            if name == "read_file" && summary == "src/main.rs"
    )));

    app.on_agent_event(AgentEvent::ToolCallFinished {
        id: "c1".to_string(),
        name: "read_file".to_string(),
        is_error: false,
        duration_ms: 42,
        result_preview: "ok".to_string(),
        result_detail: "full output".to_string(),
    });
    assert!(app.transcript().iter().any(|line| matches!(
        line,
        Line::Tool {
            state: ToolState::Ok,
            duration_ms: Some(42),
            ..
        }
    )));
    app.on_agent_event(AgentEvent::AgentFinished { steps: 2 });
    assert!(!app.busy());
}

#[test]
fn scroll_clamps_and_ctrlc_quits() {
    let mut app = App::new("s");
    for index in 0..40 {
        app.on_agent_event(AgentEvent::TextDelta {
            text: format!("line {index}\n"),
        });
    }
    app.on_agent_event(AgentEvent::AgentFinished { steps: 1 });

    app.handle_key(Key::PageUp);
    app.handle_key(Key::PageUp);
    assert_eq!(app.scroll(), 20);
    app.handle_key(Key::PageDown);
    assert_eq!(app.scroll(), 10);
    app.handle_key(Key::PageDown);
    app.handle_key(Key::PageDown);
    assert_eq!(app.scroll(), 0, "scroll tidak boleh negatif");

    assert_eq!(app.handle_key(Key::CtrlC), Action::Quit);
    assert!(app.should_quit());
}

#[test]
fn esc_clears_input() {
    let mut app = App::new("s");
    type_str(&mut app, "draft");
    app.handle_key(Key::Esc);
    assert_eq!(app.input(), "");
}
