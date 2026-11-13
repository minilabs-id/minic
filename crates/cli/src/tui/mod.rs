//! Driver TUI: alt-screen + raw mode (ratatui/crossterm).
//!
//! Logika UI ada di `tui::app` (murni, bisa diuji). Modul ini hanya:
//! - menyiapkan/memulihkan terminal (via guard RAII),
//! - menerjemahkan key & event agen ke `App`,
//! - menjalankan turn agen di task terpisah,
//! - merender transcript/composer/status dan modal approval.

pub mod app;
pub mod slash;

use crate::approval::{ApprovalChoice, ApprovalRequest};
use crate::hooks_impl::FileHooks;
use crate::model_session::ModelSession;
use crate::policy::SimplePolicy;
use crate::provider::openai_compat::{ModelEntry, OpenAiCompat};
use crate::render::text::{display_width, sanitize, truncate};
use crate::store::SessionPersistence;
use app::{
    Action, App, ApprovalView, Command, ConnectView, Key, LineKind, ModelChoice, PickerView,
};
use minic_core::agent::{HistoryPersistence, RunOptions, run_agent};
use minic_core::events::AgentEvent;
use minic_core::history::ContextStore;
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::Message;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TLine, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub struct TuiDeps {
    /// Provider/model aktif. Boleh belum siap saat TUI dibuka; `/connect`
    /// dan `/model` mengisi sesi ini tanpa restart.
    pub session: Arc<ModelSession>,
    /// Adapter HTTP untuk daftar model & context window. `None` untuk fake
    /// provider (tidak ada endpoint nyata).
    pub remote: Option<Arc<OpenAiCompat>>,
    pub base_url: String,
    pub api_key: String,
    pub registry: Arc<ToolRegistry>,
    pub policy: Arc<SimplePolicy>,
    pub hooks: Arc<FileHooks>,
    pub tool_ctx: ToolContext,
    pub system: Option<String>,
    pub max_steps: u32,
    /// Perintah verifikasi pasca-mutasi (lihat `RunOptions::verify_command`).
    pub verify_command: Option<String>,
    pub mode: String,
    pub cwd: String,
    /// Persistensi streaming: kernel menyimpan snapshot tiap akhir turn.
    pub persistence: Arc<SessionPersistence>,
    /// Permintaan approval dari agent (dijawab lewat modal, bukan stdin).
    pub approvals: UnboundedReceiver<ApprovalRequest>,
    pub verbose: bool,
}

/// Data yang dibutuhkan satu turn agent (hasil destructuring `TuiDeps` agar
/// receiver approval bisa dipinjam terpisah oleh event loop).
struct TurnDeps {
    registry: Arc<ToolRegistry>,
    policy: Arc<SimplePolicy>,
    hooks: Arc<FileHooks>,
    tool_ctx: ToolContext,
    system: Option<String>,
    max_steps: u32,
    verify_command: Option<String>,
    persistence: Arc<SessionPersistence>,
}

/// Guard yang memulihkan terminal saat di-drop (termasuk saat panic).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = std::io::stdout();
        let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, Show);
    }
}

/// Hasil pekerjaan jaringan yang diminta UI (model list / context window).
enum ModelMsg {
    Listed {
        seq: u64,
        models: Result<Vec<ModelEntry>, String>,
    },
    Context {
        seq: u64,
        model: String,
        tokens: Option<u32>,
    },
}

pub async fn run(deps: TuiDeps, initial: Vec<Message>) -> anyhow::Result<()> {
    let TuiDeps {
        session,
        remote,
        base_url,
        api_key,
        registry,
        policy,
        hooks,
        tool_ctx,
        system,
        max_steps,
        verify_command,
        mode,
        cwd,
        persistence,
        mut approvals,
        verbose,
    } = deps;
    let turns = TurnDeps {
        registry,
        policy,
        hooks,
        tool_ctx,
        system,
        max_steps,
        verify_command,
        persistence: persistence.clone(),
    };
    // Kredensial hanya hidup di memori proses; field key di form tidak pernah
    // menampilkan nilai lama.
    let mut remote = remote;
    let mut base_url = base_url;
    let mut api_key = api_key;
    // Nomor generasi provider: hasil fetch lama tidak boleh menimpa koneksi
    // baru (mis. pengguna connect dua kali saat fetch pertama lambat).
    let mut provider_seq: u64 = 0;
    // Env tetap menang atas `context_length` provider, termasuk saat model
    // diganti dari picker.
    let context_override: Option<u32> = std::env::var("MINIC_CONTEXT_TOKENS")
        .ok()
        .and_then(|value| value.parse().ok());

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _guard = TerminalGuard;

    let mut app = App::new("");
    refresh_status(&mut app, &session, &mode);
    app.set_verbose(verbose);
    app.load_history(initial);
    let (model_tx, mut model_rx) = unbounded_channel::<ModelMsg>();

    // Event input dari thread terpisah (blocking) → channel.
    let (input_tx, mut input_rx) = unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    // Setiap turn punya nomor urut: event/result dari turn yang sudah
    // dibatalkan diabaikan, jadi Ctrl+C tidak bisa "menghidupkan" output lama.
    let (agent_tx, mut agent_rx) = unbounded_channel::<(u64, AgentEvent)>();
    let (result_tx, mut result_rx) = unbounded_channel::<(u64, Vec<Message>)>();
    let mut turn_seq: u64 = 0;
    let mut active_seq: u64 = 0;
    // Pegangan task turn aktif: Ctrl+C saat busy meng-abort-nya.
    let mut turn_handle: Option<tokio::task::JoinHandle<()>> = None;
    // Balasan approval yang sedang menunggu di modal.
    let mut approval_reply: Option<tokio::sync::oneshot::Sender<ApprovalChoice>> = None;
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| draw(frame, &mut app, &cwd))?;
            dirty = false;
        }
        if app.should_quit() {
            break;
        }

        let tick_ms = if app.busy() { 100 } else { 400 };
        tokio::select! {
            maybe = input_rx.recv() => {
                let Some(event) = maybe else { break };
                if let Some(action) = handle_input(&mut app, &event) {
                    match action {
                        Action::Submit(text) => {
                            if let Some(snapshot) = session.snapshot() {
                                turn_seq += 1;
                                active_seq = turn_seq;
                                app.begin_turn();
                                turn_handle = Some(spawn_turn(
                                    &turns,
                                    &app,
                                    &agent_tx,
                                    &result_tx,
                                    active_seq,
                                    snapshot,
                                    text,
                                ));
                            } else {
                                app.note("no model connected · use /connect, then /model");
                            }
                        }
                        Action::Cancel => {
                            if let Some(handle) = turn_handle.take() {
                                handle.abort();
                            }
                            // Turn yang dibatalkan tidak boleh lagi menulis
                            // event/result ke UI.
                            turn_seq += 1;
                            active_seq = turn_seq;
                            // Drop sender → policy menerima channel closed
                            // dan menolak tool (fail-closed).
                            approval_reply = None;
                            app.cancel_turn();
                        }
                        Action::Approval(choice) => {
                            if let Some(reply) = approval_reply.take() {
                                let _ = reply.send(choice);
                            }
                            app.clear_approval();
                        }
                        Action::Command(command) => match command {
                            Command::Connect => {
                                app.open_connect(base_url.clone(), !api_key.trim().is_empty());
                            }
                            Command::Model(Some(id)) => {
                                let id = id.trim().to_string();
                                if id.is_empty() {
                                    app.note("model id is empty");
                                } else {
                                    // Env `MINIC_CONTEXT_TOKENS` menang atas
                                    // nilai provider; tanpa itu context lama
                                    // dipertahankan sampai fetch selesai.
                                    session.set_model(id.clone(), context_override);
                                    refresh_status(&mut app, &session, &mode);
                                    app.note(format!("model: {id}"));
                                    // Deteksi context window best-effort; model
                                    // tetap terpasang walau endpoint gagal.
                                    if context_override.is_none()
                                        && let Some(remote) = remote.clone()
                                    {
                                        provider_seq += 1;
                                        let seq = provider_seq;
                                        let tx = model_tx.clone();
                                        tokio::spawn(async move {
                                            let tokens = remote.fetch_context_window(&id).await;
                                            let _ = tx.send(ModelMsg::Context {
                                                seq,
                                                model: id,
                                                tokens,
                                            });
                                        });
                                    }
                                }
                            }
                            Command::Model(None) => {
                                if let Some(remote) = remote.clone() {
                                    provider_seq += 1;
                                    let seq = provider_seq;
                                    app.open_picker_loading();
                                    let tx = model_tx.clone();
                                    tokio::spawn(async move {
                                        let models = remote.list_models().await;
                                        let _ = tx.send(ModelMsg::Listed { seq, models });
                                    });
                                } else if session.provider_connected() {
                                    app.note(
                                        "model list unavailable for this provider · use /model <id>",
                                    );
                                } else {
                                    app.open_connect(
                                        base_url.clone(),
                                        !api_key.trim().is_empty(),
                                    );
                                }
                            }
                            Command::Help => {}
                        },
                        Action::Connect(input) => {
                            let base = input.base_url.trim().to_string();
                            // Field key kosong = pertahankan key lama (env,
                            // config, auth, atau koneksi sebelumnya).
                            let key = if input.api_key.trim().is_empty() {
                                api_key.clone()
                            } else {
                                input.api_key.trim().to_string()
                            };
                            let openai = Arc::new(OpenAiCompat::new(base.clone(), key.clone()));
                            remote = Some(openai.clone());
                            base_url = base;
                            api_key = key;
                            session.set_provider(openai.clone());
                            refresh_status(&mut app, &session, &mode);
                            provider_seq += 1;
                            let seq = provider_seq;
                            app.open_picker_loading();
                            let tx = model_tx.clone();
                            tokio::spawn(async move {
                                let models = openai.list_models().await;
                                let _ = tx.send(ModelMsg::Listed { seq, models });
                            });
                        }
                        Action::PickModel(choice) => {
                            // Env menimpa nilai dari `/v1/models`, termasuk
                            // saat model diganti dari picker.
                            let context = context_override.or(choice.context_length);
                            session.set_model(choice.id.clone(), context);
                            refresh_status(&mut app, &session, &mode);
                            app.note(format!("model: {}", choice.id));
                            if context.is_none()
                                && let Some(remote) = remote.clone()
                            {
                                provider_seq += 1;
                                let seq = provider_seq;
                                let id = choice.id.clone();
                                let tx = model_tx.clone();
                                tokio::spawn(async move {
                                    let tokens = remote.fetch_context_window(&id).await;
                                    let _ = tx.send(ModelMsg::Context { seq, model: id, tokens });
                                });
                            }
                        }
                        Action::Quit => break,
                        Action::None => {}
                    }
                }
                dirty = true;
            }
            maybe = agent_rx.recv() => {
                if let Some((seq, event)) = maybe
                    && seq == active_seq
                {
                    app.on_agent_event(event);
                    dirty = true;
                }
            }
            maybe = approvals.recv() => {
                if let Some(request) = maybe {
                    app.prompt_approval(ApprovalView {
                        tool: request.tool,
                        summary: request.summary,
                        reason: request.reason,
                        category: request.category.label().to_string(),
                        details: request.details,
                        show_details: false,
                        detail_scroll: 0,
                    });
                    approval_reply = Some(request.reply);
                    dirty = true;
                }
            }
            maybe = result_rx.recv() => {
                if let Some((seq, messages)) = maybe
                    && seq == active_seq
                {
                    // Kernel sudah menyimpan tiap akhir turn; ini snapshot
                    // final untuk memastikan pesan terakhir ikut tersimpan.
                    persistence.save(&messages);
                    app.replace_messages(messages);
                    turn_handle = None;
                    dirty = true;
                }
            }
            maybe = model_rx.recv() => {
                if let Some(message) = maybe {
                    match message {
                        ModelMsg::Listed { seq, models } if seq == provider_seq => {
                            match models {
                                Ok(entries) => app.set_models(
                                    entries
                                        .into_iter()
                                        .map(|entry| ModelChoice {
                                            id: entry.id,
                                            context_length: entry.context_length,
                                        })
                                        .collect(),
                                ),
                                Err(error) => app.set_models_error(error),
                            }
                            dirty = true;
                        }
                        ModelMsg::Context { seq, model, tokens } if seq == provider_seq => {
                            if session.model() == model
                                && let Some(tokens) = tokens
                            {
                                session.set_model(model, Some(tokens));
                                refresh_status(&mut app, &session, &mode);
                                dirty = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(tick_ms)) => {
                if app.tick() {
                    dirty = true;
                }
            }
        }
    }

    Ok(())
}

fn refresh_status(app: &mut App, session: &ModelSession, mode: &str) {
    let model = session.model();
    let status = if !session.provider_connected() {
        format!("no provider · {mode} · /connect")
    } else if model.trim().is_empty() {
        format!("no model · {mode} · /model")
    } else {
        format!("{model} · {mode}")
    };
    app.set_status(status);
    app.set_ready(session.configured());
    app.set_context_tokens(session.context_tokens());
}

fn handle_input(app: &mut App, event: &Event) -> Option<Action> {
    match event {
        Event::Key(key) => translate(key).map(|key| app.handle_key(key)),
        Event::Paste(text) => {
            app.paste(text);
            Some(Action::None)
        }
        Event::Resize(_, _) => Some(Action::None),
        _ => None,
    }
}

fn spawn_turn(
    deps: &TurnDeps,
    app: &App,
    agent_tx: &UnboundedSender<(u64, AgentEvent)>,
    result_tx: &UnboundedSender<(u64, Vec<Message>)>,
    seq: u64,
    snapshot: crate::model_session::ModelSnapshot,
    text: String,
) -> tokio::task::JoinHandle<()> {
    let mut history = ContextStore::from_messages(app.messages().to_vec());
    history.push(Message::user(text));

    let registry = deps.registry.clone();
    let policy = deps.policy.clone();
    let hooks = deps.hooks.clone();
    let tool_ctx = deps.tool_ctx.clone();
    let system = deps.system.clone();
    let max_steps = deps.max_steps;
    let verify_command = deps.verify_command.clone();
    let persistence = deps.persistence.clone();
    let agent_tx = agent_tx.clone();
    let result_tx = result_tx.clone();

    tokio::spawn(async move {
        let options = RunOptions {
            model: snapshot.model,
            system,
            max_steps,
            max_tokens: None,
            context_window_tokens: Some(snapshot.context_tokens),
            keep_recent: minic_core::compaction::DEFAULT_KEEP_RECENT,
            verify_command: verify_command.clone(),
        };
        // Simpan prompt user sebelum panggilan model pertama: kalau provider
        // gagal/timeout, prompt tidak hilang dan sesi tetap bisa di-resume.
        persistence.save(history.messages());
        let mut on_event = |event: AgentEvent| {
            let _ = agent_tx.send((seq, event));
        };
        let outcome = run_agent(
            snapshot.provider.as_ref(),
            registry.as_ref(),
            policy.as_ref(),
            hooks.as_ref(),
            &mut history,
            &tool_ctx,
            &options,
            Some(persistence.as_ref()),
            &mut on_event,
        )
        .await;
        if let Err(error) = outcome {
            let _ = agent_tx.send((
                seq,
                AgentEvent::Error {
                    message: format!("{error}"),
                },
            ));
        }
        let _ = result_tx.send((seq, history.messages().to_vec()));
    })
}

fn translate(key: &KeyEvent) -> Option<Key> {
    // Windows mengirim Press/Release; hanya proses Press.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if ctrl {
        // Hanya shortcut yang dikenal. Ctrl+letter lain TIDAK boleh menjadi
        // karakter biasa (bug lama: Ctrl+D mengetik 'd').
        return match key.code {
            KeyCode::Char('c' | 'C') => Some(Key::CtrlC),
            KeyCode::Char('d' | 'D') => Some(Key::CtrlD),
            KeyCode::Char('u' | 'U') => Some(Key::CtrlU),
            KeyCode::Char('k' | 'K') => Some(Key::CtrlK),
            KeyCode::Char('w' | 'W') => Some(Key::CtrlW),
            KeyCode::Char('j' | 'J') => Some(Key::Newline),
            KeyCode::Enter => Some(Key::Newline),
            _ => None,
        };
    }
    if alt {
        return None;
    }
    Some(match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => Key::Newline,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Tab => Key::Tab,
        KeyCode::Esc => Key::Esc,
        _ => return None,
    })
}

fn draw(frame: &mut ratatui::Frame, app: &mut App, cwd: &str) {
    let area = frame.area();
    let composer = app.composer(area.width as usize);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(composer.height as u16),
        ])
        .split(area);

    let hidden = draw_transcript(frame, chunks[0], app, cwd);
    draw_status(frame, chunks[1], app, cwd, hidden);
    draw_composer(frame, chunks[2], app, &composer);

    // Autocomplete mengambang di atas composer. Digambar sebelum modal agar
    // modal (approval/connect/picker) selalu menang bila keduanya aktif.
    if let Some(completion) = app.completion_view() {
        draw_completion(frame, area, chunks[2], &completion);
    }

    if let Some(approval) = app.approval() {
        draw_approval(frame, area, approval);
    } else if let Some(output) = app.output_view() {
        draw_output(frame, area, output);
    } else if let Some(connect) = app.connect() {
        draw_connect(frame, area, connect);
    } else if let Some(picker) = app.picker() {
        draw_picker(frame, area, picker);
    }
}

/// Popup autocomplete slash command, mengambang tepat di atas composer.
///
/// Lebar mengikuti entri terpanjang (dibatasi), dan posisinya digeser ke atas
/// bila tidak cukup ruang — supaya daftar tidak pernah menutupi composer atau
/// keluar layar.
fn draw_completion(
    frame: &mut ratatui::Frame,
    area: Rect,
    composer_area: Rect,
    completion: &app::CompletionView,
) {
    if completion.items.is_empty() {
        return;
    }
    let dim = Style::default().add_modifier(Modifier::DIM);

    // Polos: tanpa border, tanpa judul, tanpa petunjuk — hanya daftar nama yang
    // mengambang di atas composer. Lebar mengikuti nama terpanjang (termasuk
    // penanda "❯ " dan "/"), dibatasi agar tidak keluar layar. `.clamp(low,
    // high)` panic bila low > high, jadi batas bawah diturunkan ke lebar area
    // pada layar sangat sempit.
    let widest = completion
        .items
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(10);
    let max_width = area.width.saturating_sub(1).max(1);
    let width = ((widest as u16).max(8)).min(max_width);

    let height = completion.height();
    // Daftar tidak boleh menutupi composer: naik tepat di atasnya.
    let y = composer_area.y.saturating_sub(height);
    let x = (area.x + 1).min(area.right().saturating_sub(width));
    let popup = Rect::new(x, y, width, height);
    // Sel di bawah daftar harus dibersihkan: tanpa border, sisa teks transcript
    // akan tercampur dengan nama command.
    frame.render_widget(Clear, popup);

    // Pilihan dibedakan **hanya** lewat warna (tanpa panah/penanda) dan nama
    // ditampilkan apa adanya tanpa garis miring.
    let lines: Vec<TLine> = completion
        .items
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let picked = index == completion.selected;
            let style = if picked {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            };
            TLine::from(Span::styled(*name, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), popup);
}
/// Panel baca-saja untuk hasil tool penuh. Mengisi hampir seluruh layar dan
/// hanya menggulir teks — tidak ada state agen yang bisa berubah dari sini.
fn draw_output(frame: &mut ratatui::Frame, area: Rect, output: &app::OutputView) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let width = area.width.saturating_sub(4).min(area.width);
    let height = area.height.saturating_sub(2).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(dim)
        .title(Span::styled(
            " tool output ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let body_width = inner.width.saturating_sub(2) as usize;
    let budget = inner.height.saturating_sub(2) as usize;
    let all: Vec<&str> = output.body.lines().collect();
    let total = all.len();
    let scroll = output.scroll.min(total.saturating_sub(1));

    let mut lines = vec![TLine::styled(
        format!(
            " {}",
            truncate(
                &sanitize(&output.title),
                inner.width.saturating_sub(2) as usize
            )
        ),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for line in all.iter().skip(scroll).take(budget) {
        lines.push(TLine::styled(
            format!(" {}", truncate(&sanitize(line), body_width)),
            dim,
        ));
    }

    let seen = scroll.saturating_add(budget);
    let mut hint = String::from(" Esc / q close");
    if scroll > 0 {
        hint.push_str("  \u{2191} PgUp");
    }
    if seen < total {
        hint.push_str("  \u{2193} PgDn");
    }
    lines.push(TLine::raw(hint));

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Baris transcript yang tidak terlihat di atas dan di bawah viewport.
#[derive(Debug, Clone, Copy, Default)]
struct HiddenLines {
    above: usize,
    below: usize,
}

fn draw_transcript(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut App,
    cwd: &str,
) -> HiddenLines {
    if app.is_empty() && !app.busy() {
        draw_welcome(frame, area, app, cwd);
        return HiddenLines::default();
    }
    let viewport = app.viewport(area.height as usize, area.width as usize);
    let lines: Vec<TLine> = viewport
        .lines
        .iter()
        .map(|line| TLine::styled(line.text.clone(), line_style(line.kind)))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    HiddenLines {
        above: viewport.offset,
        below: viewport.max_offset - viewport.offset,
    }
}

fn draw_welcome(frame: &mut ratatui::Frame, area: Rect, app: &App, cwd: &str) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // Layar awal sengaja bersih: hanya identitas dan konteks kerja. Tidak ada
    // penjelasan command di sini — itu tugas `/help`, dan menampilkannya terus
    // di awal hanya menambah teks yang harus dilewati mata setiap sesi.
    let mut lines = vec![
        TLine::raw(""),
        TLine::styled("  minic", Style::default().add_modifier(Modifier::BOLD)),
        TLine::styled(
            format!(
                "  {} · ctx {}%",
                truncate(&sanitize(app.status()), 60),
                app.context_percent()
            ),
            dim,
        ),
        TLine::styled(format!("  {}", truncate(&sanitize(cwd), 72)), dim),
    ];
    // Satu petunjuk saja, dan hanya bila belum siap: tanpa provider pengguna
    // akan buntu kalau tidak diberi tahu langkah pertamanya.
    if !app.ready() {
        lines.push(TLine::raw(""));
        lines.push(TLine::styled("  /connect", dim));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_status(frame: &mut ratatui::Frame, area: Rect, app: &App, cwd: &str, hidden: HiddenLines) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let left_width = area.width.saturating_sub(30).max(10) as usize;
    let mut left = if app.busy() {
        let label = app.current_tool().unwrap_or("thinking");
        format!(
            "{} {} · {}s · ctx {}%",
            app.spinner_frame(),
            label,
            app.elapsed_secs(),
            app.context_percent()
        )
    } else {
        format!("{} · ctx {}%", app.status(), app.context_percent())
    };
    // Indikator posisi scroll dua arah: pengguna harus tahu ada konten di atas
    // yang tidak terlihat, bukan hanya di bawah.
    if hidden.above > 0 {
        left.push_str(&format!(" · ↑ {}", hidden.above));
    }
    if hidden.below > 0 {
        left.push_str(&format!(" · ↓ {}", hidden.below));
    }
    let left = truncate(&sanitize(&left), left_width);

    let right = if app.busy() {
        "^C cancel".to_string()
    } else if !app.following() {
        "End: bottom".to_string()
    } else {
        truncate(&sanitize(cwd), 28)
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(30)])
        .split(area);
    frame.render_widget(Paragraph::new(left).style(dim), chunks[0]);
    frame.render_widget(
        Paragraph::new(right).style(dim).alignment(Alignment::Right),
        chunks[1],
    );
}

fn draw_composer(frame: &mut ratatui::Frame, area: Rect, app: &App, composer: &app::ComposerView) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    if app.input().is_empty() {
        // Satu hint saja. Status line sudah memberi tahu provider/model, jadi
        // composer tidak mengulanginya; ia hanya menyebut aksi berikutnya.
        let hint = if app.busy() {
            // Status line sudah menampilkan spinner + ^C cancel; composer
            // dibiarkan tenang agar layar tidak punya dua indikator sibuk.
            "  "
        } else if !app.ready() {
            "❯ /connect to set up a model…"
        } else if app.has_tool_output() {
            "❯ Type a task…  (Enter opens last tool output)"
        } else {
            "❯ Type a task…"
        };
        frame.render_widget(Paragraph::new(hint).style(dim), area);
        if !app.busy() && app.approval().is_none() && app.output_view().is_none() {
            frame.set_cursor_position(ratatui::layout::Position::new(area.x + 2, area.y));
        }
        return;
    }

    let lines: Vec<TLine> = composer
        .rows
        .iter()
        .map(|row| TLine::raw(row.clone()))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);

    if !app.busy() && app.approval().is_none() {
        let x = area
            .x
            .saturating_add(composer.cursor_col as u16)
            .min(area.right().saturating_sub(1));
        let y = area
            .y
            .saturating_add(composer.cursor_row as u16)
            .min(area.bottom().saturating_sub(1));
        frame.set_cursor_position(ratatui::layout::Position::new(x, y));
    }
}

fn draw_approval(frame: &mut ratatui::Frame, area: Rect, approval: &ApprovalView) {
    let width = area.width.saturating_sub(4).clamp(40, 76).min(area.width);
    // Saat panel argumen lengkap dibuka, modal tumbuh hingga 70% layar.
    let height = if approval.show_details {
        (area.height * 70 / 100).max(10).min(area.height)
    } else {
        9.min(area.height)
    };
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().add_modifier(Modifier::DIM))
        .title(Span::styled(
            if approval.show_details {
                " permission · arguments "
            } else {
                " permission "
            },
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let heading = if approval.summary.is_empty() {
        approval.tool.clone()
    } else {
        format!("{}  {}", approval.tool, approval.summary)
    };
    let category_line = format!(
        "category: {}   ·   [a] allows this tool, [c] allows all '{}' tools",
        approval.category, approval.category
    );

    let mut lines = vec![
        TLine::raw(""),
        TLine::styled(
            format!(
                " {}",
                truncate(&sanitize(&heading), width.saturating_sub(4) as usize)
            ),
            Style::default(),
        ),
        TLine::styled(
            format!(
                " {}",
                truncate(
                    &sanitize(&approval.reason),
                    width.saturating_sub(4) as usize
                )
            ),
            Style::default().add_modifier(Modifier::DIM),
        ),
        TLine::styled(
            format!(
                " {}",
                truncate(&sanitize(&category_line), width.saturating_sub(4) as usize)
            ),
            Style::default().add_modifier(Modifier::DIM),
        ),
        TLine::raw(""),
    ];

    if approval.show_details {
        let body_width = width.saturating_sub(4) as usize;
        // Anggaran baris argumen: `inner` sudah dikurangi border. Sisa tetap:
        // baris kosong awal + heading + reason + category + baris kosong + hint.
        let fixed_lines = 6u16;
        let budget = inner.height.saturating_sub(fixed_lines) as usize;
        let lines_total = approval.details.lines().count();
        for line in approval
            .details
            .lines()
            .skip(approval.detail_scroll)
            .take(budget)
        {
            lines.push(TLine::styled(
                format!(" {}", truncate(line, body_width)),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        let seen = approval.detail_scroll.saturating_add(budget);
        let mut hint = String::from(" ? / Esc close");
        if approval.detail_scroll > 0 {
            hint.push_str("  ↑ PgUp");
        }
        if seen < lines_total {
            hint.push_str("  ↓ PgDn");
        }
        lines.push(TLine::raw(hint));
    } else {
        lines.push(TLine::raw(
            " [y] once  [a] this tool  [c] all in category  [n] deny  [?] arguments".to_string(),
        ));
    }

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_connect(frame: &mut ratatui::Frame, area: Rect, connect: &ConnectView) {
    let width = area.width.saturating_sub(4).clamp(40, 76).min(area.width);
    let height = 11.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().add_modifier(Modifier::DIM))
        .title(Span::styled(
            " connect provider ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let dim = Style::default().add_modifier(Modifier::DIM);
    let active = Style::default().add_modifier(Modifier::BOLD);
    let error_style = modal_error_style();
    let field_width = inner.width.saturating_sub(2) as usize;
    let base = truncate(&sanitize(&connect.base_url), field_width);
    let key = if connect.api_key.is_empty() {
        if connect.has_key {
            "(existing key kept · leave blank)".to_string()
        } else {
            "(optional · leave blank if none)".to_string()
        }
    } else {
        "•".repeat(connect.api_key.chars().count().min(40))
    };

    let mut lines = vec![
        TLine::raw(""),
        TLine::styled(" Base URL", if connect.focus == 0 { active } else { dim }),
        TLine::raw(format!("  {base}")),
        TLine::raw(""),
        TLine::styled(" API key", if connect.focus == 1 { active } else { dim }),
        TLine::styled(format!("  {key}"), dim),
        TLine::raw(""),
    ];
    if let Some(error) = &connect.error {
        lines.push(TLine::styled(
            format!(" {}", truncate(&sanitize(error), field_width)),
            error_style,
        ));
    } else {
        lines.push(TLine::raw(""));
    }
    lines.push(TLine::styled(
        " Tab switch · Enter connect · Esc cancel",
        dim,
    ));
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);

    // Cursor mengikuti field aktif; untuk API key kolom dihitung dari masker.
    if inner.width > 4 && inner.height > 6 {
        let text = connect.active_text();
        let cursor = connect.active_cursor();
        let row = if connect.focus == 0 { 2u16 } else { 5u16 };
        let mut cursor = cursor.min(text.len());
        while cursor > 0 && !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let col = if connect.focus == 0 {
            display_width(&text[..cursor])
        } else {
            text[..cursor].chars().count().min(40)
        };
        let field_cols = inner.width.saturating_sub(4) as usize;
        let cursor_x = inner
            .x
            .saturating_add(2)
            .saturating_add(col.min(field_cols) as u16)
            .min(inner.right().saturating_sub(1));
        let cursor_y = inner
            .y
            .saturating_add(row)
            .min(inner.bottom().saturating_sub(1));
        frame.set_cursor_position(ratatui::layout::Position::new(cursor_x, cursor_y));
    }
}

fn draw_picker(frame: &mut ratatui::Frame, area: Rect, picker: &PickerView) {
    let width = area.width.saturating_sub(4).clamp(40, 76).min(area.width);
    let height = 16.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().add_modifier(Modifier::DIM))
        .title(Span::styled(
            " model ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let dim = Style::default().add_modifier(Modifier::DIM);
    let selected = Style::default().add_modifier(Modifier::BOLD);
    let filter = truncate(
        &sanitize(&picker.query),
        inner.width.saturating_sub(18) as usize,
    );
    let mut lines = vec![
        TLine::styled(
            format!(" / filter: {filter}   ({} match)", picker.match_count()),
            dim,
        ),
        TLine::raw(""),
    ];

    let list_rows = inner.height.saturating_sub(4).max(1) as usize;
    if picker.loading {
        lines.push(TLine::styled(" fetching models…", dim));
    } else {
        let rows = picker.visible(list_rows);
        if rows.is_empty() {
            lines.push(TLine::styled(
                " no matching models · type an id and press Enter",
                dim,
            ));
        } else {
            for (id, is_selected) in rows {
                let id = truncate(&sanitize(&id), inner.width.saturating_sub(3) as usize);
                if is_selected {
                    lines.push(TLine::styled(format!(" › {id}"), selected));
                } else {
                    lines.push(TLine::raw(format!("   {id}")));
                }
            }
        }
    }

    if let Some(error) = &picker.error {
        lines.push(TLine::styled(
            format!(
                " {}",
                truncate(&sanitize(error), inner.width.saturating_sub(3) as usize)
            ),
            modal_error_style(),
        ));
    }
    lines.push(TLine::styled(
        " ↑↓ move · type filter · Enter apply · Esc cancel",
        dim,
    ));
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn modal_error_style() -> Style {
    if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    }
}

fn line_style(kind: LineKind) -> Style {
    if no_color() {
        return match kind {
            LineKind::User => Style::default().add_modifier(Modifier::BOLD),
            LineKind::Error | LineKind::ToolError => Style::default().add_modifier(Modifier::BOLD),
            LineKind::Reasoning | LineKind::Note => Style::default().add_modifier(Modifier::DIM),
            _ => Style::default(),
        };
    }
    match kind {
        LineKind::User => Style::default().fg(Color::Cyan),
        LineKind::Assistant => Style::default(),
        LineKind::Reasoning => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        LineKind::ToolRunning => Style::default().fg(Color::DarkGray),
        LineKind::ToolOk => Style::default().fg(Color::Green),
        LineKind::ToolError | LineKind::Error => Style::default().fg(Color::Red),
        LineKind::Note => Style::default().fg(Color::DarkGray),
        LineKind::Blank => Style::default(),
    }
}

fn no_color() -> bool {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some())
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn approval(show_details: bool, details: &str) -> ApprovalView {
        ApprovalView {
            tool: "bash".into(),
            summary: "git push origin main".into(),
            reason: "shell command is publishes: git push origin main".into(),
            category: "shell".into(),
            details: details.to_string(),
            show_details,
            detail_scroll: 0,
        }
    }

    /// Modal harus bisa digambar di ukuran layar kecil tanpa panic, dan teks
    /// penting (tool, kategori, tombol) harus benar-benar muncul di buffer.
    #[test]
    fn approval_modal_renders_at_small_and_normal_sizes() {
        for (width, height) in [(20u16, 6u16), (40, 10), (80, 24), (120, 40)] {
            let mut terminal =
                Terminal::new(TestBackend::new(width, height)).expect("test backend");
            let view = approval(false, "{\n  \"command\": \"git push\"\n}");
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    draw_approval(frame, area, &view);
                })
                .expect("draw must not panic");
        }
    }

    #[test]
    fn approval_modal_shows_category_and_keys() {
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("test backend");
        let view = approval(false, "");
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_approval(frame, area, &view);
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("bash"), "tool name visible: {text}");
        assert!(text.contains("shell"), "category visible: {text}");
        assert!(text.contains("[c]"), "category key hint visible: {text}");
        assert!(text.contains("[?]"), "details key hint visible: {text}");
    }

    #[test]
    fn approval_details_panel_renders_arguments_and_scrolls() {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test backend");
        let mut view = approval(true, "{\n  \"command\": \"git push origin main\"\n}");
        view.detail_scroll = 1;
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_approval(frame, area, &view);
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("git push origin main"),
            "arguments visible: {text}"
        );
        // Setelah scroll ke bawah, hint harus menunjukkan arah naik.
        assert!(text.contains("PgUp"), "scroll hint visible: {text}");
        assert!(text.contains("Esc"), "close hint visible: {text}");
    }
}

#[cfg(test)]
mod render_snapshot_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Cetak modal ke stdout (test dijalankan dengan `--nocapture`) supaya
    /// tata letaknya bisa diperiksa manusia, bukan hanya "tidak panic".
    #[test]
    fn print_approval_modal_layout() {
        let mut terminal = Terminal::new(TestBackend::new(84, 14)).expect("backend");
        let view = ApprovalView {
            tool: "bash".into(),
            summary: "git push origin main".into(),
            reason: "shell command is publishes: git push origin main".into(),
            category: "shell".into(),
            details: "{
  \"command\": \"git push origin main\"
}"
            .to_string(),
            show_details: true,
            detail_scroll: 0,
        };
        terminal
            .draw(|frame| draw_approval(frame, frame.area(), &view))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            println!("|{row}|");
        }
    }
}

#[cfg(test)]
mod output_render_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn body() -> String {
        (1..=25)
            .map(|i| format!("line {i}: cargo test output line"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn output_panel_renders_without_panic_at_small_sizes() {
        for (width, height) in [(20u16, 5u16), (40, 10), (100, 30)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
            let view = app::OutputView {
                title: "bash  cargo test".into(),
                body: body(),
                scroll: 0,
            };
            terminal
                .draw(|frame| draw_output(frame, frame.area(), &view))
                .expect("draw must not panic");
        }
    }

    #[test]
    fn print_output_panel_layout() {
        let mut terminal = Terminal::new(TestBackend::new(76, 12)).expect("backend");
        let view = app::OutputView {
            title: "bash  cargo test".into(),
            body: body(),
            scroll: 3,
        };
        terminal
            .draw(|frame| draw_output(frame, frame.area(), &view))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            println!("|{row}|");
        }
    }

    fn completion_view_for(selected: usize) -> app::CompletionView {
        app::CompletionView {
            items: crate::tui::slash::COMMANDS
                .iter()
                .map(|cmd| cmd.name)
                .collect(),
            selected,
            hint: Some("CMD"),
        }
    }

    /// Popup harus bisa digambar di layar sekecil apa pun tanpa panic, dan
    /// tidak boleh menimpa composer (baris tempat pengguna mengetik).
    #[test]
    fn completion_popup_renders_at_small_and_normal_sizes() {
        for (width, height) in [(20u16, 6u16), (40, 10), (80, 24), (120, 40)] {
            let mut terminal =
                Terminal::new(TestBackend::new(width, height)).expect("test backend");
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    let composer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
                    draw_completion(frame, area, composer, &completion_view_for(0));
                })
                .expect("draw must not panic");
        }
    }

    #[test]
    fn completion_popup_lists_commands_and_marks_the_selection() {
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).expect("test backend");
        terminal
            .draw(|frame| {
                let area = frame.area();
                let composer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
                draw_completion(frame, area, composer, &completion_view_for(1));
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        // Nama tampil apa adanya: tanpa garis miring, tanpa panah.
        assert!(text.contains("model"), "nama command terlihat: {text}");
        assert!(text.contains("connect"), "nama command terlihat: {text}");
        for absent in [
            "❯", "/model", "/connect", "│", "─", "commands", "Tab", "Esc", "↑↓",
        ] {
            assert!(
                !text.contains(absent),
                "daftar harus polos, tapi memuat {absent:?}: {text}"
            );
        }

        // Pilihan dibedakan **hanya** lewat warna: baris terpilih memakai warna
        // aksen, baris lain tidak. Tidak ada penanda/panah sama sekali.
        let named_rows: Vec<(u16, String, ratatui::style::Color)> = (0..buffer.area.height)
            .map(|row| {
                // Ambil warna sel pertama yang berisi karakter: kolom 0 adalah
                // margin kosong hasil `Clear`, bukan teks daftar.
                let mut fg = ratatui::style::Color::Reset;
                let mut found = false;
                let mut line = String::new();
                for x in 0..buffer.area.width {
                    let cell = &buffer[(x, row)];
                    if !found && cell.symbol() != " " {
                        fg = cell.fg;
                        found = true;
                    }
                    line.push_str(cell.symbol());
                }
                (row, line.trim_end().to_string(), fg)
            })
            .filter(|(_, line, _)| !line.is_empty())
            .collect();
        let row_of = |needle: &str| {
            named_rows
                .iter()
                .find(|(_, line, _)| line.contains(needle))
                .unwrap_or_else(|| panic!("baris {needle:?} tidak ada: {named_rows:?}"))
        };
        // `selected` = 1 -> baris kedua (`connect`) berwarna, sisanya tidak.
        assert_eq!(
            row_of("connect").2,
            ratatui::style::Color::Cyan,
            "baris terpilih harus berwarna aksen, bukan ditandai panah: {named_rows:?}"
        );
        for needle in ["model", "verify"] {
            assert_ne!(
                row_of(needle).2,
                ratatui::style::Color::Cyan,
                "baris tidak terpilih tidak boleh berwarna aksen: {named_rows:?}"
            );
        }
    }

    /// Popup harus berada **di atas** composer, bukan menimpanya.
    #[test]
    fn completion_popup_sits_above_the_composer_row() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test backend");
        // Baris composer terakhir kita isi penanda yang harus tetap terlihat.
        terminal
            .draw(|frame| {
                let area = frame.area();
                let composer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
                frame.render_widget(Paragraph::new("COMPOSER-HERE"), composer);
                draw_completion(frame, area, composer, &completion_view_for(0));
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let last_row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, buffer.area.height - 1)].symbol())
            .collect();
        assert!(
            last_row.contains("COMPOSER-HERE"),
            "popup menimpa composer: {last_row}"
        );
    }

    #[test]
    fn completion_popup_is_scroll_safe_when_items_exceed_space() {
        // Layar pendek: popup harus memotong daftar, bukan menggambar keluar area.
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).expect("test backend");
        terminal
            .draw(|frame| {
                let area = frame.area();
                let composer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
                draw_completion(frame, area, composer, &completion_view_for(0));
            })
            .expect("draw must not panic");
    }

    /// Cetak layout untuk verifikasi manual (jalankan dengan `--nocapture`).
    #[test]
    fn print_completion_layout() {
        let mut terminal = Terminal::new(TestBackend::new(74, 18)).expect("test backend");
        terminal
            .draw(|frame| {
                let area = frame.area();
                let composer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
                frame.render_widget(Paragraph::new("❯ /mo"), composer);
                draw_completion(frame, area, composer, &completion_view_for(0));
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            println!("|{row}|");
        }
    }
}
