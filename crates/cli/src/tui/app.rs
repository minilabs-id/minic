//! State machine TUI (murni, tanpa terminal I/O).
//!
//! Prinsip desain: bersih dan sederhana.
//! - Transcript selalu mengikuti ekor kecuali pengguna scroll ke atas.
//! - Composer multi-baris dengan wrap berbasis lebar kolom (CJK/emoji benar).
//! - Satu baris per tool call (di-update saat selesai), bukan dua baris.
//! - Approval adalah modal di dalam TUI; tidak pernah membaca stdin langsung.
//! - Semua teks tak tepercaya dibersihkan sebelum ditampilkan.

use crate::approval::{ApprovalChoice, Category};
use crate::render::text::{display_width, format_duration, preview_line, sanitize, summarize_tool};
use crate::tui::slash::{COMMANDS, Completion};
use minic_core::events::{AgentEvent, DecisionKind};
use minic_core::tokens::estimate_messages;
use minic_core::types::{Message, Role};
use std::collections::HashMap;
use std::time::Instant;
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    /// Baris baru di composer (Shift+Enter / Ctrl+J).
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Esc,
    CtrlC,
    CtrlD,
    CtrlU,
    CtrlK,
    CtrlW,
}

/// Aksi yang diminta UI ke driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Submit(String),
    /// Hentikan turn yang sedang berjalan; aplikasi tetap hidup.
    Cancel,
    Approval(ApprovalChoice),
    /// Perintah slash yang butuh I/O jaringan/terminal (driver yang eksekusi).
    Command(Command),
    /// Form connect disubmit: driver membuat provider lalu mengambil model.
    Connect(ConnectInput),
    /// Model dipilih dari picker (atau diketik manual saat daftar kosong).
    PickModel(ModelChoice),
    Quit,
}

/// Perintah slash yang dikenali composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Buka form base URL + API key.
    Connect,
    /// Ambil daftar model (tanpa argumen) atau set model langsung (dengan id).
    Model(Option<String>),
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    User(String),
    Assistant(String),
    Reasoning(String),
    Tool {
        name: String,
        summary: String,
        state: ToolState,
        duration_ms: Option<u64>,
        error: Option<String>,
        /// Hasil penuh untuk panel detail (`Enter` pada baris tool). Kosong
        /// selama tool masih berjalan.
        detail: String,
    },
    Note(String),
    Error(String),
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    Reasoning,
    ToolRunning,
    ToolOk,
    ToolError,
    Note,
    Error,
    Blank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    pub kind: LineKind,
    pub text: String,
}

/// Data approval yang ditampilkan modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalView {
    pub tool: String,
    pub summary: String,
    pub reason: String,
    /// Kategori tool (`read`, `file-write`, `shell`, `other`) — membuat cakupan
    /// tombol `a`/`A` terlihat jelas.
    pub category: String,
    /// Argumen lengkap (sudah di-sanitize), ditampilkan saat pengguna menekan `?`.
    pub details: String,
    /// `true` saat panel argumen lengkap sedang dibuka.
    pub show_details: bool,
    /// Offset scroll untuk panel argumen lengkap.
    pub detail_scroll: usize,
}

/// Form koneksi provider. API key lama **tidak pernah** ditampilkan; field
/// kosong berarti "pertahankan key yang sudah ada".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectView {
    pub base_url: String,
    pub api_key: String,
    /// 0 = base URL, 1 = API key.
    pub focus: usize,
    pub has_key: bool,
    pub error: Option<String>,
    base_cursor: usize,
    key_cursor: usize,
}

/// Jawaban form connect. `api_key` kosong = pakai key lama.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectInput {
    pub base_url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    pub context_length: Option<u32>,
}

/// Picker model: daftar dari `/v1/models`, bisa difilter dengan mengetik.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerView {
    pub models: Vec<ModelChoice>,
    pub query: String,
    pub selected: usize,
    pub error: Option<String>,
    pub loading: bool,
}

impl PickerView {
    fn filtered(&self) -> Vec<usize> {
        let query = self.query.trim().to_ascii_lowercase();
        self.models
            .iter()
            .enumerate()
            .filter(|(_, model)| query.is_empty() || model.id.to_ascii_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect()
    }

    fn choice(&self) -> Option<ModelChoice> {
        let filtered = self.filtered();
        let index = *filtered.get(self.selected)?;
        self.models.get(index).cloned()
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.filtered().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected.min(count - 1) as isize;
        self.selected = (current + delta).clamp(0, count as isize - 1) as usize;
    }

    /// Baris yang terlihat beserta status terpilih, dipusatkan di sekitar
    /// pilihan agar daftar panjang tetap nyaman.
    pub fn visible(&self, limit: usize) -> Vec<(String, bool)> {
        let filtered = self.filtered();
        if filtered.is_empty() || limit == 0 {
            return Vec::new();
        }
        let selected = self.selected.min(filtered.len() - 1);
        let mut start = selected.saturating_sub(limit / 2);
        if start + limit > filtered.len() {
            start = filtered.len().saturating_sub(limit);
        }
        let end = (start + limit).min(filtered.len());
        filtered[start..end]
            .iter()
            .enumerate()
            .map(|(offset, &index)| (self.models[index].id.clone(), start + offset == selected))
            .collect()
    }

    pub fn match_count(&self) -> usize {
        self.filtered().len()
    }
}

impl ConnectView {
    pub fn active_text(&self) -> &str {
        if self.focus == 0 {
            &self.base_url
        } else {
            &self.api_key
        }
    }

    pub fn active_cursor(&self) -> usize {
        if self.focus == 0 {
            self.base_cursor
        } else {
            self.key_cursor
        }
    }
}

#[derive(Debug, Clone)]
struct Seg {
    start: usize,
    end: usize,
    /// Segmen terakhir dari baris keras (diakhiri `\n` di sumber).
    hard: bool,
}

pub struct Viewport {
    pub lines: Vec<DisplayLine>,
    pub offset: usize,
    pub max_offset: usize,
}

pub struct ComposerView {
    pub rows: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub height: usize,
}

/// Isi popup autocomplete. Dipisah dari `App` agar renderer tidak perlu tahu
/// soal state filter — ia hanya menggambar apa yang sudah dipilih.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionView {
    /// Nama command tanpa garis miring. Daftar sengaja tanpa keterangan:
    /// cukup nama, supaya popup tetap kecil dan mudah dipindai.
    pub items: Vec<&'static str>,
    pub selected: usize,
    /// Petunjuk argumen untuk entri terpilih, mis. `CMD`.
    pub hint: Option<&'static str>,
}

impl CompletionView {
    /// Tinggi popup: satu baris per entri + satu baris petunjuk.
    pub fn height(&self) -> u16 {
        (self.items.len() as u16).saturating_add(1)
    }
}

/// Panel baca-saja untuk hasil tool penuh. Tidak pernah mengubah state agen;
/// hanya membungkus teks yang sudah diterima UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputView {
    pub title: String,
    pub body: String,
    /// Baris yang sudah digulir dari atas.
    pub scroll: usize,
}

const MAX_COMPOSER_ROWS: usize = 6;

pub struct App {
    messages: Vec<Message>,
    transcript: Vec<Line>,
    input: String,
    cursor: usize,
    assistant_buf: String,
    reasoning_buf: String,
    busy: bool,
    should_quit: bool,
    /// Jarak dari bawah (0 = mengikuti ekor).
    scroll_from_bottom: usize,
    follow: bool,
    status: String,
    context_tokens: u32,
    ctx_used: u64,
    verbose: bool,
    turn_started: Option<Instant>,
    current_tool: Option<String>,
    tool_index: HashMap<String, usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    approval: Option<ApprovalView>,
    /// Panel baca-saja untuk hasil tool penuh (`Enter` pada baris tool).
    output_view: Option<OutputView>,
    connect: Option<ConnectView>,
    picker: Option<PickerView>,
    /// Provider + model siap; composer boleh mengirim task.
    ready: bool,
    /// Autocomplete slash command yang mengambang di atas composer.
    completion: Completion,
    spinner: u8,
}

impl App {
    pub fn new(status: impl Into<String>) -> Self {
        Self {
            messages: Vec::new(),
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            assistant_buf: String::new(),
            reasoning_buf: String::new(),
            busy: false,
            should_quit: false,
            scroll_from_bottom: 0,
            follow: true,
            status: status.into(),
            context_tokens: 128_000,
            ctx_used: 0,
            verbose: false,
            turn_started: None,
            current_tool: None,
            tool_index: HashMap::new(),
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            approval: None,
            output_view: None,
            connect: None,
            picker: None,
            // Default true agar state machine tetap bisa diuji tanpa driver.
            // Driver memanggil `set_ready` sesuai status provider.
            ready: true,
            completion: Completion::new(),
            spinner: 0,
        }
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_context_tokens(&mut self, tokens: u32) {
        self.context_tokens = tokens.max(1);
    }

    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn scroll(&self) -> usize {
        self.scroll_from_bottom
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn transcript(&self) -> &[Line] {
        &self.transcript
    }

    pub fn assistant_live(&self) -> &str {
        &self.assistant_buf
    }

    pub fn approval(&self) -> Option<&ApprovalView> {
        self.approval.as_ref()
    }

    pub fn connect(&self) -> Option<&ConnectView> {
        self.connect.as_ref()
    }

    pub fn picker(&self) -> Option<&PickerView> {
        self.picker.as_ref()
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    /// Daftar autocomplete yang harus dirender, bila terbuka.
    pub fn completion_view(&self) -> Option<CompletionView> {
        if !self.completion.is_open() {
            return None;
        }
        Some(CompletionView {
            items: self
                .completion
                .visible()
                .into_iter()
                .map(|cmd| cmd.name)
                .collect(),
            selected: self.completion.selected_index(),
            hint: self.completion.hint(),
        })
    }

    pub fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }

    /// Buka form koneksi. Key lama tidak disalin ke field (tidak pernah
    /// ditampilkan); `has_key` hanya menandai bahwa kosong = pertahankan.
    pub fn open_connect(&mut self, base_url: impl Into<String>, has_key: bool) {
        let base_url = base_url.into();
        self.picker = None;
        self.connect = Some(ConnectView {
            base_cursor: base_url.len(),
            base_url,
            api_key: String::new(),
            focus: 0,
            has_key,
            error: None,
            key_cursor: 0,
        });
    }

    /// Buka picker dalam keadaan memuat; driver mengisi hasilnya nanti.
    pub fn open_picker_loading(&mut self) {
        self.connect = None;
        self.picker = Some(PickerView {
            models: Vec::new(),
            query: String::new(),
            selected: 0,
            error: None,
            loading: true,
        });
    }

    pub fn set_models(&mut self, models: Vec<ModelChoice>) {
        if let Some(picker) = &mut self.picker {
            picker.models = models;
            picker.selected = 0;
            picker.loading = false;
            picker.error = None;
        }
    }

    pub fn set_models_error(&mut self, error: impl Into<String>) {
        if let Some(picker) = &mut self.picker {
            picker.models.clear();
            picker.selected = 0;
            picker.loading = false;
            picker.error = Some(error.into());
        }
    }

    pub fn close_modal(&mut self) {
        self.connect = None;
        self.picker = None;
    }

    // ── Panel hasil tool (baca-saja) ────────────────────────────────

    pub fn output_view(&self) -> Option<&OutputView> {
        self.output_view.as_ref()
    }

    /// Buka hasil tool **terakhir** yang punya detail. Mengembalikan `false`
    /// bila belum ada tool yang selesai (composer kosong tetap tidak melakukan
    /// apa-apa).
    pub fn open_last_tool_output(&mut self) -> bool {
        let found = self.transcript.iter().rev().find_map(|line| match line {
            Line::Tool {
                name,
                summary,
                detail,
                ..
            } if !detail.is_empty() => Some((name.clone(), summary.clone(), detail.clone())),
            _ => None,
        });
        let Some((name, summary, body)) = found else {
            return false;
        };
        let title = if summary.is_empty() {
            name
        } else {
            format!("{name}  {summary}")
        };
        self.output_view = Some(OutputView {
            title,
            body,
            scroll: 0,
        });
        true
    }

    pub fn close_output(&mut self) {
        self.output_view = None;
    }

    /// Apakah ada baris tool yang punya detail (untuk hint composer).
    pub fn has_tool_output(&self) -> bool {
        self.transcript
            .iter()
            .any(|line| matches!(line, Line::Tool { detail, .. } if !detail.is_empty()))
    }

    fn handle_output_key(&mut self, key: Key) -> Action {
        let Some(view) = self.output_view.as_mut() else {
            return Action::None;
        };
        match key {
            Key::Esc | Key::Char('q') | Key::Char('Q') | Key::Enter | Key::CtrlC => {
                self.output_view = None;
            }
            Key::PageUp | Key::Char('k') => {
                view.scroll = view.scroll.saturating_sub(10);
            }
            Key::Up => {
                view.scroll = view.scroll.saturating_sub(1);
            }
            Key::PageDown | Key::Char('j') => {
                view.scroll = view.scroll.saturating_add(10);
            }
            Key::Down => {
                view.scroll = view.scroll.saturating_add(1);
            }
            Key::Home => view.scroll = 0,
            Key::End => view.scroll = view.body.lines().count(),
            _ => {}
        }
        Action::None
    }

    /// Catatan satu baris di transcript (dipakai driver untuk hasil /model).
    pub fn note(&mut self, text: impl Into<String>) {
        self.push_note(text);
        self.scroll_to_bottom();
    }

    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.ctx_used = estimate_messages(&messages);
        self.messages = messages;
    }

    /// Muat history sesi yang di-resume: isi transcript dari pesan lama agar
    /// pengguna melihat konteks sebelumnya, bukan layar kosong.
    pub fn load_history(&mut self, messages: Vec<Message>) {
        self.ctx_used = estimate_messages(&messages);
        self.transcript.clear();
        for message in &messages {
            match message.role {
                Role::User => {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        self.push_blank();
                        self.transcript.push(Line::User(text));
                    }
                }
                Role::Assistant => {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        self.transcript.push(Line::Assistant(text));
                    }
                }
                Role::Tool => {
                    let name = message.name.clone().unwrap_or_else(|| "tool".to_string());
                    let error = message
                        .is_error
                        .then(|| preview_line(&message.text()))
                        .filter(|preview| !preview.is_empty());
                    self.transcript.push(Line::Tool {
                        name,
                        summary: String::new(),
                        state: if message.is_error {
                            ToolState::Error
                        } else {
                            ToolState::Ok
                        },
                        duration_ms: None,
                        error,
                        // Hasil lama dari sesi yang di-resume tetap bisa dibuka.
                        detail: message.text(),
                    });
                }
                Role::System => {}
            }
        }
        self.messages = messages;
        self.push_blank();
        self.scroll_to_bottom();
    }

    pub fn is_empty(&self) -> bool {
        self.transcript
            .iter()
            .all(|line| matches!(line, Line::Blank))
            && self.assistant_buf.is_empty()
            && self.reasoning_buf.is_empty()
    }

    // ── Key handling ────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: Key) -> Action {
        if self.approval.is_some() {
            return self.handle_approval_key(key);
        }
        if self.output_view.is_some() {
            return self.handle_output_key(key);
        }
        if self.connect.is_some() {
            return self.handle_connect_key(key);
        }
        if self.picker.is_some() {
            return self.handle_picker_key(key);
        }

        match key {
            Key::CtrlC => {
                // Saat streaming: Ctrl+C membatalkan turn (tetap di TUI).
                // Saat idle: Ctrl+C keluar seperti biasa.
                if self.busy {
                    Action::Cancel
                } else {
                    self.should_quit = true;
                    Action::Quit
                }
            }
            Key::CtrlD => {
                if self.busy {
                    Action::Cancel
                } else {
                    self.should_quit = true;
                    Action::Quit
                }
            }
            Key::PageUp => {
                self.scroll_up(10);
                Action::None
            }
            Key::PageDown => {
                self.scroll_down(10);
                Action::None
            }
            _ if self.busy => Action::None,
            Key::Enter => {
                // Daftar autocomplete terbuka: Enter memilih entri, bukan
                // mengirim. Tanpa ini, menekan Enter saat daftar tampil akan
                // menjalankan command yang salah/ setengah jadi.
                if self.completion.is_open() {
                    self.accept_completion();
                    return Action::None;
                }
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    // Composer kosong: buka hasil tool terakhir (baca-saja).
                    // Ini pintu untuk melihat output `bash` penuh tanpa
                    // menambah satu tombol global baru.
                    if self.open_last_tool_output() {
                        return Action::None;
                    }
                    return Action::None;
                }
                // Slash command bukan prompt: jangan masuk transcript/history.
                if text.starts_with('/') {
                    self.input.clear();
                    self.cursor = 0;
                    self.history_index = None;
                    self.draft.clear();
                    return self.run_command(&text);
                }
                if !self.ready {
                    self.input.clear();
                    self.cursor = 0;
                    self.history_index = None;
                    self.push_note("no model connected · use /connect, then /model");
                    self.scroll_to_bottom();
                    return Action::None;
                }
                if self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                self.history_index = None;
                self.draft.clear();
                self.input.clear();
                self.cursor = 0;
                if !self.transcript.is_empty() {
                    self.push_blank();
                }
                self.transcript.push(Line::User(text.clone()));
                self.scroll_to_bottom();
                Action::Submit(text)
            }
            Key::Newline => {
                self.insert_text("\n");
                Action::None
            }
            Key::Char(c) => {
                if !c.is_control() {
                    self.insert_char(c);
                }
                Action::None
            }
            Key::Backspace => {
                self.backspace();
                Action::None
            }
            Key::Delete => {
                self.delete_forward();
                Action::None
            }
            Key::Left => {
                self.move_left();
                Action::None
            }
            Key::Right => {
                self.move_right();
                Action::None
            }
            Key::Up => {
                if self.completion.is_open() {
                    self.completion.move_by(-1);
                    return Action::None;
                }
                self.history_prev();
                Action::None
            }
            Key::Down => {
                if self.completion.is_open() {
                    self.completion.move_by(1);
                    return Action::None;
                }
                self.history_next();
                Action::None
            }
            Key::Home => {
                self.cursor = self.line_start();
                Action::None
            }
            Key::End => {
                self.cursor = self.line_end();
                Action::None
            }
            Key::Tab => {
                // Tab menyelesaikan nama command tanpa menjalankannya.
                if self.completion.is_open() {
                    self.accept_completion();
                }
                Action::None
            }
            Key::Esc => {
                // Esc menutup daftar lebih dulu; baru Esc kedua membersihkan
                // composer. Kalau tidak, Esc saat daftar terbuka membuang teks
                // yang sudah diketik.
                if self.completion.is_open() {
                    self.completion.close();
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.history_index = None;
                }
                Action::None
            }
            Key::CtrlU => {
                let start = self.line_start();
                self.input.replace_range(start..self.cursor, "");
                self.cursor = start;
                Action::None
            }
            Key::CtrlK => {
                let end = self.line_end();
                self.input.replace_range(self.cursor..end, "");
                Action::None
            }
            Key::CtrlW => {
                self.delete_word();
                Action::None
            }
        }
    }

    /// Tempel teks (bracketed paste): newline dipertahankan, kontrol lain
    /// dibuang, tab menjadi spasi.
    pub fn paste(&mut self, text: &str) {
        let cleaned = sanitize(text);
        if let Some(connect) = self.connect.as_mut() {
            // Form satu baris: newline paste menjadi spasi.
            let single = cleaned.replace(['\n', '\r'], " ");
            let (field, cursor) = active_field(connect);
            field_insert(field, cursor, &single);
            return;
        }
        if let Some(picker) = self.picker.as_mut() {
            if !picker.loading {
                picker
                    .query
                    .push_str(cleaned.replace(['\n', '\r'], " ").trim());
                picker.selected = 0;
            }
            return;
        }
        if self.busy || self.approval.is_some() {
            return;
        }
        self.insert_text(&cleaned);
    }

    /// Terima entri autocomplete yang sedang dipilih.
    ///
    /// Command berargumen: isi `/name ` dan biarkan daftar tertutup, supaya
    /// pengguna langsung mengetik argumennya. Command tanpa argumen: isi
    /// `/name` lengkap dan tutup daftar — Enter berikutnya menjalankannya.
    fn accept_completion(&mut self) {
        let Some(command) = self.completion.selected_command() else {
            self.completion.close();
            return;
        };
        let text = command.insert_text();
        self.input = text;
        self.cursor = self.input.len();
        self.completion.close();
        self.reset_history_nav();
    }

    fn run_command(&mut self, text: &str) -> Action {
        let mut parts = text.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();
        match command {
            "/connect" | "/provider" | "/login" => Action::Command(Command::Connect),
            "/model" | "/models" => {
                Action::Command(Command::Model((!rest.is_empty()).then(|| rest.to_string())))
            }
            "/help" => {
                self.push_help();
                Action::None
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
                Action::Quit
            }
            _ => {
                // Saran diambil dari registry, jadi tidak bisa basi saat
                // command baru ditambahkan.
                let names: Vec<String> = COMMANDS.iter().map(|c| format!("/{}", c.name)).collect();
                self.note(format!(
                    "unknown command {command} · try {}",
                    names.join(" · ")
                ));
                Action::None
            }
        }
    }

    fn push_help(&mut self) {
        self.push_blank();
        // Daftar command dibangun dari registry: menambah command otomatis
        // muncul di bantuan, tanpa menyunting string di sini.
        let listing = COMMANDS
            .iter()
            .map(|c| match c.args {
                crate::tui::slash::ArgKind::None => format!("/{}", c.name),
                crate::tui::slash::ArgKind::Required(arg) => format!("/{} <{arg}>", c.name),
            })
            .collect::<Vec<_>>()
            .join(" · ");
        self.push_note(format!("commands: {listing}"));
        self.push_note(
            "keys: Enter submit · Ctrl+J newline · ↑↓ history/autocomplete · Tab complete · PgUp/PgDn scroll · Ctrl+C cancel/quit",
        );
        self.push_note(
            "approval: y once · a this tool · c whole category · n deny · ? full arguments",
        );
        self.scroll_to_bottom();
    }

    fn handle_connect_key(&mut self, key: Key) -> Action {
        let mut submit: Option<ConnectInput> = None;
        let mut close = false;
        {
            let Some(connect) = self.connect.as_mut() else {
                return Action::None;
            };
            match key {
                Key::Esc | Key::CtrlC => close = true,
                Key::Tab | Key::Up | Key::Down => {
                    connect.focus = 1 - connect.focus.min(1);
                    connect.error = None;
                }
                Key::Enter => {
                    let base = connect.base_url.trim().to_string();
                    if base.is_empty() {
                        connect.focus = 0;
                        connect.error = Some("base URL is required".to_string());
                    } else {
                        submit = Some(ConnectInput {
                            base_url: base,
                            api_key: connect.api_key.trim().to_string(),
                        });
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    let mut buf = [0u8; 4];
                    let (field, cursor) = active_field(connect);
                    field_insert(field, cursor, c.encode_utf8(&mut buf));
                    connect.error = None;
                }
                Key::Backspace => {
                    let (field, cursor) = active_field(connect);
                    field_backspace(field, cursor);
                }
                Key::Delete => {
                    let (field, cursor) = active_field(connect);
                    field_delete(field, cursor);
                }
                Key::Left => {
                    let (field, cursor) = active_field(connect);
                    field_left(field, cursor);
                }
                Key::Right => {
                    let (field, cursor) = active_field(connect);
                    field_right(field, cursor);
                }
                Key::Home => {
                    let (_, cursor) = active_field(connect);
                    *cursor = 0;
                }
                Key::End => {
                    let (field, cursor) = active_field(connect);
                    *cursor = field.len();
                }
                Key::CtrlU => {
                    let (field, cursor) = active_field(connect);
                    field.clear();
                    *cursor = 0;
                }
                Key::CtrlK => {
                    let (field, cursor) = active_field(connect);
                    field.truncate(*cursor);
                }
                Key::CtrlW => {
                    let (field, cursor) = active_field(connect);
                    field_delete_word(field, cursor);
                }
                _ => {}
            }
        }
        if close {
            self.connect = None;
        }
        if let Some(input) = submit {
            self.connect = None;
            return Action::Connect(input);
        }
        Action::None
    }

    fn handle_picker_key(&mut self, key: Key) -> Action {
        let mut picked: Option<ModelChoice> = None;
        let mut close = false;
        {
            let Some(picker) = self.picker.as_mut() else {
                return Action::None;
            };
            if picker.loading {
                if matches!(key, Key::Esc | Key::CtrlC) {
                    close = true;
                }
            } else {
                match key {
                    Key::Esc | Key::CtrlC => close = true,
                    Key::Up => picker.move_selection(-1),
                    Key::Down => picker.move_selection(1),
                    Key::Char(c) if !c.is_control() => {
                        picker.query.push(c);
                        picker.selected = 0;
                    }
                    Key::Backspace => {
                        picker.query.pop();
                        picker.selected = 0;
                    }
                    Key::CtrlU => {
                        picker.query.clear();
                        picker.selected = 0;
                    }
                    Key::Enter => {
                        if let Some(choice) = picker.choice() {
                            picked = Some(choice);
                        } else if !picker.query.trim().is_empty() {
                            // Provider tanpa `/v1/models` (atau daftar kosong):
                            // id yang diketik langsung dipakai.
                            picked = Some(ModelChoice {
                                id: picker.query.trim().to_string(),
                                context_length: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        if close {
            self.picker = None;
        }
        if let Some(choice) = picked {
            self.picker = None;
            return Action::PickModel(choice);
        }
        Action::None
    }

    // ── Agent events ────────────────────────────────────────────────

    pub fn on_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { .. } => {
                if self.turn_started.is_none() {
                    self.turn_started = Some(Instant::now());
                }
            }
            AgentEvent::TextDelta { text } => {
                self.flush_reasoning();
                self.assistant_buf.push_str(&text);
            }
            AgentEvent::ReasoningDelta { text } => {
                if self.verbose {
                    self.reasoning_buf.push_str(&text);
                }
            }
            AgentEvent::ToolCallStarted {
                id,
                name,
                arguments,
            } => {
                self.flush_assistant();
                self.flush_reasoning();
                let summary = summarize_tool(&name, &arguments);
                self.current_tool = Some(if summary.is_empty() {
                    name.clone()
                } else {
                    format!("{name}  {summary}")
                });
                self.tool_index.insert(id, self.transcript.len());
                self.transcript.push(Line::Tool {
                    name,
                    summary,
                    state: ToolState::Running,
                    duration_ms: None,
                    error: None,
                    detail: String::new(),
                });
            }
            AgentEvent::ToolCallFinished {
                id,
                name,
                is_error,
                duration_ms,
                result_preview,
                result_detail,
            } => {
                let error = is_error
                    .then(|| preview_line(&result_preview))
                    .filter(|p| !p.is_empty());
                if let Some(&index) = self.tool_index.get(&id)
                    && let Some(Line::Tool {
                        state,
                        duration_ms: duration,
                        error: stored,
                        detail: stored_detail,
                        ..
                    }) = self.transcript.get_mut(index)
                {
                    *state = if is_error {
                        ToolState::Error
                    } else {
                        ToolState::Ok
                    };
                    *duration = Some(duration_ms);
                    *stored = error;
                    *stored_detail = result_detail;
                } else {
                    self.transcript.push(Line::Tool {
                        name,
                        summary: String::new(),
                        state: if is_error {
                            ToolState::Error
                        } else {
                            ToolState::Ok
                        },
                        duration_ms: Some(duration_ms),
                        error,
                        detail: result_detail,
                    });
                }
                self.current_tool = None;
            }
            AgentEvent::TurnFinished { .. } => {}
            AgentEvent::AgentFinished { .. } => {
                self.flush_assistant();
                self.flush_reasoning();
                self.busy = false;
                self.turn_started = None;
                self.current_tool = None;
                self.push_blank();
            }
            AgentEvent::ContextCompacted { elided } => {
                self.flush_assistant();
                self.push_note(format!(
                    "context compacted · {elided} older message(s) folded"
                ));
            }
            AgentEvent::Note { text } => {
                self.flush_assistant();
                self.push_note(text);
            }
            AgentEvent::ToolDecided {
                name,
                decision,
                reason,
                ..
            } => {
                // Alasan penolakan ditampilkan menetap di transcript: tanpa
                // ini pengguna hanya melihat tool gagal tanpa tahu siapa yang
                // menolak (policy, jail, atau hook).
                if decision == DecisionKind::Deny {
                    self.flush_assistant();
                    let detail = if reason.is_empty() {
                        "denied".to_string()
                    } else {
                        format!("denied · {reason}")
                    };
                    self.push_note(format!("⊘ {name} {detail}"));
                }
            }
            AgentEvent::Error { message } => {
                self.flush_assistant();
                self.flush_reasoning();
                self.transcript.push(Line::Error(message));
                self.busy = false;
                self.turn_started = None;
                self.current_tool = None;
                self.push_blank();
            }
        }
    }

    pub fn begin_turn(&mut self) {
        self.busy = true;
        self.turn_started = Some(Instant::now());
        self.current_tool = None;
        self.tool_index.clear();
        self.scroll_to_bottom();
    }

    /// Turn dibatalkan pengguna (Ctrl+C saat busy): tampilkan teks parsial,
    /// catat pembatalan, dan kembalikan UI ke idle.
    pub fn cancel_turn(&mut self) {
        self.flush_assistant();
        self.flush_reasoning();
        self.push_note("cancelled");
        self.busy = false;
        self.turn_started = None;
        self.current_tool = None;
        self.push_blank();
    }

    pub fn prompt_approval(&mut self, view: ApprovalView) {
        self.approval = Some(view);
    }

    pub fn clear_approval(&mut self) {
        self.approval = None;
    }

    /// Tolak/persetujuan modal. Saat panel `?` terbuka, tombol `?` menutupnya
    /// dan PgUp/PgDn menggulirnya — keputusan tidak terpicu oleh scroll.
    fn handle_approval_key(&mut self, key: Key) -> Action {
        let Some(view) = self.approval.as_mut() else {
            return Action::None;
        };
        if view.show_details {
            match key {
                Key::Char('?') | Key::Esc => {
                    view.show_details = false;
                    view.detail_scroll = 0;
                    return Action::None;
                }
                Key::PageUp => {
                    view.detail_scroll = view.detail_scroll.saturating_sub(10);
                    return Action::None;
                }
                Key::PageDown => {
                    view.detail_scroll = view.detail_scroll.saturating_add(10);
                    return Action::None;
                }
                // Semua tombol keputusan tetap bekerja dari panel detail.
                _ => {}
            }
        }
        match key {
            Key::Char('y') | Key::Char('Y') => Action::Approval(ApprovalChoice::AllowOnce),
            Key::Char('a') | Key::Char('A') => Action::Approval(ApprovalChoice::AllowAlways),
            // Shift+A = izinkan seluruh kategori (mis. semua file-write).
            Key::Char('c') | Key::Char('C') => {
                let category = Category::of_tool(&view.tool);
                Action::Approval(ApprovalChoice::AllowCategory(category))
            }
            Key::Char('?') => {
                view.show_details = true;
                Action::None
            }
            Key::Char('n') | Key::Char('N') | Key::Esc | Key::CtrlC => {
                Action::Approval(ApprovalChoice::Deny)
            }
            _ => Action::None,
        }
    }

    // ── Status ──────────────────────────────────────────────────────

    pub fn tick(&mut self) -> bool {
        if self.busy {
            self.spinner = self.spinner.wrapping_add(1);
            true
        } else {
            false
        }
    }

    pub fn spinner_frame(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[(self.spinner as usize) % FRAMES.len()]
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.turn_started
            .map(|start| start.elapsed().as_secs())
            .unwrap_or(0)
    }

    pub fn current_tool(&self) -> Option<&str> {
        self.current_tool.as_deref()
    }

    pub fn context_percent(&self) -> u16 {
        if self.context_tokens == 0 {
            return 0;
        }
        ((self.ctx_used.saturating_mul(100) / self.context_tokens as u64).min(100)) as u16
    }

    // ── Scroll ──────────────────────────────────────────────────────

    pub fn following(&self) -> bool {
        self.follow
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(amount);
        self.follow = false;
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(amount);
        if self.scroll_from_bottom == 0 {
            self.follow = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
        self.follow = true;
    }

    // ── Rendering model ─────────────────────────────────────────────

    /// Susun seluruh baris tampilan (sudah di-wrap) untuk lebar tertentu.
    pub fn wrapped(&self, width: usize) -> Vec<DisplayLine> {
        let width = width.max(1);
        let mut out = Vec::new();
        for line in &self.transcript {
            self.append_line(&mut out, line, width);
        }
        if !self.reasoning_buf.trim().is_empty() {
            let line = Line::Reasoning(self.reasoning_buf.clone());
            self.append_line(&mut out, &line, width);
        }
        if !self.assistant_buf.is_empty() {
            let line = Line::Assistant(self.assistant_buf.clone());
            self.append_line(&mut out, &line, width);
        }
        out
    }

    /// Potongan yang terlihat, bottom-anchored. Offset di-clamp ke batas atas
    /// agar PgUp berulang di posisi teratas tidak menumpuk jarak tak terlihat.
    pub fn viewport(&mut self, height: usize, width: usize) -> Viewport {
        let wrapped = self.wrapped(width);
        let max_offset = wrapped.len().saturating_sub(height.max(1));
        self.scroll_from_bottom = self.scroll_from_bottom.min(max_offset);
        let offset = max_offset - self.scroll_from_bottom;
        let end = (offset + height).min(wrapped.len());
        Viewport {
            lines: wrapped[offset..end].to_vec(),
            offset,
            max_offset,
        }
    }

    /// Composer: baris terlihat, posisi cursor, dan tinggi area.
    pub fn composer(&self, width: usize) -> ComposerView {
        let inner = width.saturating_sub(2).max(1);
        let (segments, cursor_row, cursor_col) = self.composer_state(inner);
        let height = segments.len().clamp(1, MAX_COMPOSER_ROWS);
        let window_start = if cursor_row >= height {
            cursor_row + 1 - height
        } else {
            0
        };
        let rows: Vec<String> = segments[window_start..(window_start + height).min(segments.len())]
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let absolute = window_start + index;
                let prefix = if absolute == 0 { "❯ " } else { "  " };
                format!("{prefix}{text}")
            })
            .collect();
        ComposerView {
            rows,
            cursor_row: cursor_row - window_start,
            cursor_col: cursor_col + 2,
            height,
        }
    }

    fn composer_state(&self, width: usize) -> (Vec<String>, usize, usize) {
        let segments = input_segments(&self.input, width);
        let mut row = 0usize;
        for (index, seg) in segments.iter().enumerate() {
            let after = self.cursor > seg.end
                || (self.cursor == seg.end && (seg.hard || index + 1 < segments.len()));
            if !after && self.cursor >= seg.start {
                row = index;
                break;
            }
            row = index;
        }
        let seg = &segments[row];
        let end = self.cursor.clamp(seg.start, seg.end);
        let col = display_width(&self.input[seg.start..end]);
        let rows = segments
            .iter()
            .map(|seg| self.input[seg.start..seg.end].to_string())
            .collect();
        (rows, row, col)
    }

    fn append_line(&self, out: &mut Vec<DisplayLine>, line: &Line, width: usize) {
        match line {
            Line::User(text) => self.push_block(out, LineKind::User, text, "❯ ", "  ", width),
            Line::Assistant(text) => {
                self.push_block(out, LineKind::Assistant, text, "  ", "  ", width)
            }
            Line::Reasoning(text) => {
                self.push_block(out, LineKind::Reasoning, text, "  ", "  ", width)
            }
            Line::Tool {
                name,
                summary,
                state,
                duration_ms,
                error,
                ..
            } => {
                let (kind, glyph) = match state {
                    ToolState::Running => (LineKind::ToolRunning, "⚙"),
                    ToolState::Ok => (LineKind::ToolOk, "✓"),
                    ToolState::Error => (LineKind::ToolError, "✗"),
                };
                let mut heading = String::from(glyph);
                heading.push(' ');
                heading.push_str(name);
                if !summary.is_empty() {
                    heading.push_str("  ");
                    heading.push_str(summary);
                }
                if let Some(ms) = duration_ms
                    && *state != ToolState::Running
                {
                    heading.push_str("  ");
                    heading.push_str(&format_duration(*ms));
                }
                self.push_block(out, kind, &heading, "  ", "    ", width);
                if let Some(error) = error
                    && !error.is_empty()
                {
                    self.push_block(out, LineKind::Error, error, "    ", "    ", width);
                }
            }
            Line::Note(text) => self.push_block(out, LineKind::Note, text, "  · ", "    ", width),
            Line::Error(text) => self.push_block(out, LineKind::Error, text, "  ! ", "    ", width),
            Line::Blank => out.push(DisplayLine {
                kind: LineKind::Blank,
                text: String::new(),
            }),
        }
    }

    /// Satu blok = satu pesan; newline keras di dalamnya tetap jadi baris
    /// terpisah, soft-wrap mengikuti lebar.
    fn push_block(
        &self,
        out: &mut Vec<DisplayLine>,
        kind: LineKind,
        text: &str,
        first_prefix: &str,
        next_prefix: &str,
        width: usize,
    ) {
        let cleaned = sanitize(text);
        for (index, hard) in cleaned.split('\n').enumerate() {
            let prefix = if index == 0 {
                first_prefix
            } else {
                next_prefix
            };
            let first_cols = width.saturating_sub(display_width(prefix)).max(1);
            let rest_cols = width.saturating_sub(display_width(next_prefix)).max(1);
            for (chunk_index, chunk) in wrap_body(hard, first_cols, rest_cols).iter().enumerate() {
                let line_prefix = if chunk_index == 0 {
                    prefix
                } else {
                    next_prefix
                };
                out.push(DisplayLine {
                    kind,
                    text: format!("{line_prefix}{chunk}"),
                });
            }
        }
    }

    // ── Internal: transcript mutation ───────────────────────────────

    fn push_blank(&mut self) {
        if !matches!(self.transcript.last(), Some(Line::Blank) | None) {
            self.transcript.push(Line::Blank);
        }
    }

    fn push_note(&mut self, text: impl Into<String>) {
        self.transcript.push(Line::Note(text.into()));
    }

    fn flush_assistant(&mut self) {
        let text = self.assistant_buf.trim_end_matches('\n').to_string();
        if !text.trim().is_empty() {
            self.transcript.push(Line::Assistant(text));
        }
        self.assistant_buf.clear();
    }

    fn flush_reasoning(&mut self) {
        let text = self.reasoning_buf.trim_end_matches('\n').to_string();
        if self.verbose && !text.trim().is_empty() {
            self.transcript.push(Line::Reasoning(text));
        }
        self.reasoning_buf.clear();
    }

    // ── Internal: composer editing ──────────────────────────────────

    fn reset_history_nav(&mut self) {
        self.history_index = None;
    }

    fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_text(c.encode_utf8(&mut buf));
    }

    fn insert_text(&mut self, text: &str) {
        self.reset_history_nav();
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.sync_completion();
    }

    /// Segarkan daftar autocomplete setelah composer berubah. Satu tempat
    /// supaya setiap jalur mutasi (ketik, hapus, tempel, history) konsisten.
    fn sync_completion(&mut self) {
        if self.busy || self.approval.is_some() {
            self.completion.close();
            return;
        }
        self.completion.refresh(&self.input, self.cursor);
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.reset_history_nav();
        let prev = self.input[..self.cursor].chars().next_back();
        if let Some(c) = prev {
            let n = c.len_utf8();
            self.input.replace_range(self.cursor - n..self.cursor, "");
            self.cursor -= n;
        }
        self.sync_completion();
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        self.reset_history_nav();
        let next = self.input[self.cursor..].chars().next();
        if let Some(c) = next {
            self.input
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
        self.sync_completion();
    }

    fn move_left(&mut self) {
        if self.cursor > 0
            && let Some((index, _)) = self.input[..self.cursor].char_indices().next_back()
        {
            self.cursor = index;
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.input.len()
            && let Some(c) = self.input[self.cursor..].chars().next()
        {
            self.cursor += c.len_utf8();
        }
    }

    fn line_start(&self) -> usize {
        self.input[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.input[self.cursor..]
            .find('\n')
            .map(|offset| self.cursor + offset)
            .unwrap_or(self.input.len())
    }

    fn delete_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.reset_history_nav();
        let before = &self.input[..self.cursor];
        let trimmed = before.trim_end_matches(char::is_whitespace);
        let cut = trimmed
            .rfind(char::is_whitespace)
            .map(|index| index + 1)
            .unwrap_or(0);
        self.input.replace_range(cut..self.cursor, "");
        self.cursor = cut;
        self.sync_completion();
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.draft = self.input.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        self.input = self.history[index].clone();
        self.cursor = self.input.len();
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.input = std::mem::take(&mut self.draft);
        }
        self.cursor = self.input.len();
    }
}

/// Field aktif pada form connect: (teks, cursor byte).
fn active_field(connect: &mut ConnectView) -> (&mut String, &mut usize) {
    if connect.focus == 0 {
        (&mut connect.base_url, &mut connect.base_cursor)
    } else {
        (&mut connect.api_key, &mut connect.key_cursor)
    }
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn field_insert(text: &mut String, cursor: &mut usize, value: &str) {
    let at = floor_char_boundary(text, *cursor);
    text.insert_str(at, value);
    *cursor = at + value.len();
}

fn field_backspace(text: &mut String, cursor: &mut usize) {
    let at = floor_char_boundary(text, *cursor);
    if at == 0 {
        return;
    }
    let previous = text[..at]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0);
    text.replace_range(previous..at, "");
    *cursor = previous;
}

fn field_delete(text: &mut String, cursor: &mut usize) {
    let at = floor_char_boundary(text, *cursor);
    if at >= text.len() {
        return;
    }
    let next = text[at..]
        .chars()
        .next()
        .map(|c| at + c.len_utf8())
        .unwrap_or(at);
    text.replace_range(at..next, "");
}

fn field_left(text: &str, cursor: &mut usize) {
    let at = floor_char_boundary(text, *cursor);
    if let Some((index, _)) = text[..at].char_indices().next_back() {
        *cursor = index;
    }
}

fn field_right(text: &str, cursor: &mut usize) {
    let at = floor_char_boundary(text, *cursor);
    if let Some(c) = text[at..].chars().next() {
        *cursor = at + c.len_utf8();
    }
}

fn field_delete_word(text: &mut String, cursor: &mut usize) {
    let at = floor_char_boundary(text, *cursor);
    if at == 0 {
        return;
    }
    let before = &text[..at];
    let trimmed = before.trim_end_matches(char::is_whitespace);
    let cut = trimmed
        .rfind(char::is_whitespace)
        .map(|index| index + 1)
        .unwrap_or(0);
    text.replace_range(cut..at, "");
    *cursor = cut;
}

/// Segmen input untuk composer: hard line + soft wrap, dengan byte range.
fn input_segments(text: &str, width: usize) -> Vec<Seg> {
    let width = width.max(1);
    let mut segments = Vec::new();
    let mut base = 0usize;
    let parts: Vec<&str> = text.split('\n').collect();
    for (part_index, part) in parts.iter().enumerate() {
        let hard_break = part_index + 1 < parts.len();
        let ranges = exact_ranges(part, width);
        let last = ranges.len().saturating_sub(1);
        for (index, (start, end)) in ranges.into_iter().enumerate() {
            segments.push(Seg {
                start: base + start,
                end: base + end,
                hard: hard_break && index == last,
            });
        }
        base += part.len() + 1;
    }
    if segments.is_empty() {
        segments.push(Seg {
            start: 0,
            end: 0,
            hard: false,
        });
    }
    segments
}

/// Range byte per baris visual; setiap byte tetap tercakup (tanpa membuang
/// spasi) supaya pemetaan cursor selalu konsisten.
fn exact_ranges(text: &str, width: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }
    let width = width.max(1);
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut col = 0usize;
        let mut end = start;
        for (offset, c) in text[start..].char_indices() {
            let w = c.width().unwrap_or(0);
            if end > start && col + w > width {
                break;
            }
            col += w;
            end = start + offset + c.len_utf8();
            if col >= width {
                break;
            }
        }
        if end == start {
            end = start
                + text[start..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
        }
        out.push((start, end));
        start = end;
    }
    out
}

/// Wrap teks biasa: utamakan pemutusan di spasi (prosa), tetapi jangan
/// membuang indentasi di awal baris (kode).
fn wrap_body(text: &str, first_cols: usize, rest_cols: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut limit = first_cols.max(1);
    while start < text.len() {
        let mut col = 0usize;
        let mut end = start;
        let mut last_space: Option<(usize, usize)> = None;
        for (offset, c) in text[start..].char_indices() {
            let w = c.width().unwrap_or(0);
            if end > start && col + w > limit {
                break;
            }
            if c == ' ' {
                last_space = Some((start + offset, col));
            }
            col += w;
            end = start + offset + c.len_utf8();
        }
        if end == start {
            end = start
                + text[start..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
        } else if end < text.len()
            && let Some((space_index, space_col)) = last_space
            && space_col * 2 >= limit
        {
            end = space_index;
        }
        out.push(text[start..end].trim_end().to_string());
        start = end;
        // Buang satu spasi pemutus; indentasi lain dipertahankan.
        if start < text.len() && text[start..].starts_with(' ') {
            start += 1;
        }
        limit = rest_cols.max(1);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(Key::Char(c));
        }
    }

    #[test]
    fn ctrl_c_quits_when_idle_and_cancels_when_busy() {
        let mut app = App::new("test");
        assert_eq!(app.handle_key(Key::CtrlC), Action::Quit);
        assert!(app.should_quit());

        let mut app = App::new("test");
        app.begin_turn();
        assert_eq!(app.handle_key(Key::CtrlC), Action::Cancel);
        assert!(!app.should_quit());
    }

    #[test]
    fn ctrl_shortcuts_do_not_insert_letters() {
        let mut app = App::new("test");
        type_str(&mut app, "ab");
        app.handle_key(Key::CtrlD);
        // Ctrl+D saat idle = keluar, bukan mengetik 'd'.
        assert!(app.should_quit());
        assert_eq!(app.input(), "ab");
    }

    #[test]
    fn multiline_and_paste_keep_newlines() {
        let mut app = App::new("test");
        app.paste("line1\r\nline2\tend");
        assert_eq!(app.input(), "line1\nline2    end");
        assert_eq!(app.handle_key(Key::Newline), Action::None);
        type_str(&mut app, "line3");
        assert!(app.input().ends_with("\nline3"));
    }

    #[test]
    fn submit_clears_and_pushes_history() {
        let mut app = App::new("test");
        type_str(&mut app, "hello");
        assert_eq!(app.handle_key(Key::Enter), Action::Submit("hello".into()));
        assert!(app.input().is_empty());
        app.handle_key(Key::Up);
        assert_eq!(app.input(), "hello");
        app.handle_key(Key::Down);
        assert_eq!(app.input(), "");
    }

    #[test]
    fn scroll_is_bottom_anchored_and_clamped() {
        let mut app = App::new("test");
        for index in 0..50 {
            app.transcript
                .push(Line::Assistant(format!("line {index}")));
        }
        let view = app.viewport(10, 80);
        assert_eq!(view.offset, view.max_offset);
        assert!(view.lines.last().unwrap().text.contains("line 49"));

        app.scroll_up(5);
        let view = app.viewport(10, 80);
        assert_eq!(view.offset, view.max_offset - 5);
        assert!(view.lines.last().unwrap().text.contains("line 44"));

        // Jauh melewati atas: tetap clamp.
        app.scroll_up(10_000);
        let view = app.viewport(10, 80);
        assert_eq!(view.offset, 0);

        app.scroll_down(10_000);
        let view = app.viewport(10, 80);
        assert_eq!(view.offset, view.max_offset);
        assert!(app.following());
    }

    #[test]
    fn tool_line_updates_in_place() {
        let mut app = App::new("test");
        app.begin_turn();
        app.on_agent_event(AgentEvent::ToolCallStarted {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        });
        let count = app.transcript().len();
        app.on_agent_event(AgentEvent::ToolCallFinished {
            id: "c1".into(),
            name: "read_file".into(),
            is_error: false,
            duration_ms: 120,
            result_preview: "ok".into(),
            result_detail: "ok".into(),
        });
        assert_eq!(app.transcript().len(), count, "tidak boleh duplikat");
        assert!(matches!(
            app.transcript().last(),
            Some(Line::Tool { state: ToolState::Ok, summary, .. }) if summary == "src/main.rs"
        ));
    }

    /// Bentuk `ApprovalView` untuk test (kategori `shell`, tanpa argumen detail).
    fn approval_view(tool: &str, summary: &str) -> ApprovalView {
        ApprovalView {
            tool: tool.to_string(),
            summary: summary.to_string(),
            reason: "mutates".to_string(),
            category: Category::of_tool(tool).label().to_string(),
            details: String::new(),
            show_details: false,
            detail_scroll: 0,
        }
    }

    #[test]
    fn approval_keys_are_modal() {
        let mut app = App::new("test");
        app.prompt_approval(approval_view("bash", "cargo test"));
        assert_eq!(
            app.handle_key(Key::Char('a')),
            Action::Approval(ApprovalChoice::AllowAlways)
        );
        assert_eq!(
            app.handle_key(Key::Esc),
            Action::Approval(ApprovalChoice::Deny)
        );
        // Key lain tidak bocor ke composer.
        assert_eq!(app.handle_key(Key::Char('x')), Action::None);
        assert!(app.input().is_empty());
    }

    #[test]
    fn approval_c_grants_the_whole_category() {
        let mut app = App::new("test");
        app.prompt_approval(approval_view("write_file", "a.rs"));
        assert_eq!(
            app.handle_key(Key::Char('c')),
            Action::Approval(ApprovalChoice::AllowCategory(Category::FileWrite))
        );
    }

    #[test]
    fn approval_details_toggle_with_question_mark() {
        let mut app = App::new("test");
        let mut view = approval_view("bash", "git push");
        view.details = "{\n  \"command\": \"git push\"\n}".to_string();
        app.prompt_approval(view);

        // `?` membuka panel argumen tanpa memicu keputusan.
        assert_eq!(app.handle_key(Key::Char('?')), Action::None);
        assert!(app.approval().expect("modal").show_details);

        // PgDn/PgUp menggulir, bukan menyetujui.
        assert_eq!(app.handle_key(Key::PageDown), Action::None);
        assert_eq!(app.approval().expect("modal").detail_scroll, 10);
        assert_eq!(app.handle_key(Key::PageUp), Action::None);
        assert_eq!(app.approval().expect("modal").detail_scroll, 0);

        // `?` lagi menutup panel, bukan deny.
        assert_eq!(app.handle_key(Key::Char('?')), Action::None);
        assert!(!app.approval().expect("modal").show_details);

        // Keputusan masih bisa diambil dari panel: `y` = allow once.
        assert_eq!(app.handle_key(Key::Char('?')), Action::None);
        assert_eq!(
            app.handle_key(Key::Char('y')),
            Action::Approval(ApprovalChoice::AllowOnce)
        );
    }

    #[test]
    fn resumed_history_is_rendered() {
        let mut app = App::new("test");
        app.load_history(vec![
            Message::user("hello"),
            Message::assistant("hi"),
            Message::tool_result(minic_core::types::ToolResult::ok(
                "c1",
                "read_file",
                "contents",
            )),
        ]);
        assert!(!app.is_empty());
        assert!(
            app.transcript()
                .iter()
                .any(|line| matches!(line, Line::User(t) if t == "hello"))
        );
        assert!(
            app.transcript()
                .iter()
                .any(|line| matches!(line, Line::Assistant(t) if t == "hi"))
        );
        assert!(app.transcript().iter().any(|line| matches!(
            line,
            Line::Tool { name, state: ToolState::Ok, .. } if name == "read_file"
        )));
    }

    #[test]
    fn connect_form_collects_provider_and_key() {
        let mut app = App::new("test");
        app.open_connect("https://api.example/v1", false);
        type_str(&mut app, "x");
        app.handle_key(Key::Tab);
        type_str(&mut app, "secret");
        assert_eq!(
            app.handle_key(Key::Enter),
            Action::Connect(ConnectInput {
                base_url: "https://api.example/v1x".to_string(),
                api_key: "secret".to_string(),
            })
        );
        assert!(app.connect().is_none(), "form tertutup setelah submit");
    }

    #[test]
    fn model_picker_filters_and_selects() {
        let mut app = App::new("test");
        app.open_picker_loading();
        app.set_models(vec![
            ModelChoice {
                id: "alpha".into(),
                context_length: None,
            },
            ModelChoice {
                id: "banana".into(),
                context_length: Some(64_000),
            },
            ModelChoice {
                id: "beta".into(),
                context_length: None,
            },
        ]);
        type_str(&mut app, "ban");
        assert_eq!(app.picker().unwrap().match_count(), 1);
        assert_eq!(
            app.handle_key(Key::Enter),
            Action::PickModel(ModelChoice {
                id: "banana".into(),
                context_length: Some(64_000),
            })
        );
        assert!(app.picker().is_none());
    }

    #[test]
    fn picker_accepts_manual_model_when_list_is_empty() {
        let mut app = App::new("test");
        app.open_picker_loading();
        app.set_models_error("http 404");
        assert_eq!(app.picker().unwrap().error.as_deref(), Some("http 404"));
        type_str(&mut app, "custom-model");
        assert_eq!(
            app.handle_key(Key::Enter),
            Action::PickModel(ModelChoice {
                id: "custom-model".into(),
                context_length: None,
            })
        );
    }

    #[test]
    fn submit_without_model_is_blocked_with_hint() {
        let mut app = App::new("test");
        app.set_ready(false);
        type_str(&mut app, "do work");
        assert_eq!(app.handle_key(Key::Enter), Action::None);
        assert!(app.input().is_empty());
        assert!(
            app.transcript()
                .iter()
                .any(|line| matches!(line, Line::Note(text) if text.contains("/connect")))
        );
    }

    #[test]
    fn slash_commands_dispatch_without_becoming_prompts() {
        let mut app = App::new("test");
        app.set_ready(false);

        type_str(&mut app, "/connect");
        assert_eq!(
            app.handle_key(Key::Enter),
            Action::Command(Command::Connect)
        );

        type_str(&mut app, "/model gpt-4o-mini");
        assert_eq!(
            app.handle_key(Key::Enter),
            Action::Command(Command::Model(Some("gpt-4o-mini".to_string())))
        );

        type_str(&mut app, "/model");
        assert_eq!(
            app.handle_key(Key::Enter),
            Action::Command(Command::Model(None))
        );

        assert!(
            !app.transcript()
                .iter()
                .any(|line| matches!(line, Line::User(text) if text.starts_with('/'))),
            "slash command tidak boleh masuk transcript prompt"
        );
    }

    #[test]
    fn composer_wraps_by_display_width() {
        let mut app = App::new("test");
        type_str(&mut app, "日本語日本語");
        let view = app.composer(7); // prefix 2 + 5 kolom
        assert!(view.rows.len() > 1);
        assert!(view.rows[0].starts_with("❯ "));
        assert!(view.rows[1].starts_with("  "));
        assert!(view.cursor_col <= 7);
    }
}

#[cfg(test)]
mod output_panel_tests {
    use super::*;

    fn finished(app: &mut App, id: &str, name: &str, detail: &str, is_error: bool) {
        app.on_agent_event(AgentEvent::ToolCallStarted {
            id: id.into(),
            name: name.into(),
            arguments: serde_json::json!({"command": "cargo test"}),
        });
        app.on_agent_event(AgentEvent::ToolCallFinished {
            id: id.into(),
            name: name.into(),
            is_error,
            duration_ms: 5,
            result_preview: "first line".into(),
            result_detail: detail.into(),
        });
    }

    #[test]
    fn enter_on_empty_composer_opens_last_tool_output() {
        let mut app = App::new("test");
        app.set_ready(true);
        finished(&mut app, "c1", "bash", "line1\nline2\nline3", true);

        assert!(app.has_tool_output());
        assert!(app.open_last_tool_output());
        let view = app.output_view().expect("panel open");
        assert_eq!(view.title, "bash  cargo test");
        assert!(view.body.contains("line3"));
        // Composer kosong: Enter di dalam panel menutupnya, tidak mengirim
        // prompt kosong dan tidak membocorkan keystroke ke composer.
        assert_eq!(app.handle_key(Key::Enter), Action::None);
        assert!(app.output_view().is_none(), "Enter menutup panel");
        assert!(app.input().is_empty());
    }

    #[test]
    fn enter_without_tool_output_does_nothing() {
        let mut app = App::new("test");
        app.set_ready(true);
        assert!(!app.has_tool_output());
        assert_eq!(app.handle_key(Key::Enter), Action::None);
        assert!(app.output_view().is_none());
    }

    #[test]
    fn output_panel_scrolls_and_closes() {
        let mut app = App::new("test");
        app.set_ready(true);
        let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
        finished(&mut app, "c1", "bash", &body, false);
        app.open_last_tool_output();

        app.handle_key(Key::PageDown);
        assert_eq!(app.output_view().expect("open").scroll, 10);
        app.handle_key(Key::End);
        assert_eq!(app.output_view().expect("open").scroll, 40);
        app.handle_key(Key::Home);
        assert_eq!(app.output_view().expect("open").scroll, 0);
        app.handle_key(Key::PageUp);
        assert_eq!(app.output_view().expect("open").scroll, 0, "clamped");

        // Esc menutup panel, bukan mengosongkan input.
        app.handle_key(Key::Esc);
        assert!(app.output_view().is_none());
    }

    #[test]
    fn typing_is_ignored_while_output_panel_is_open() {
        let mut app = App::new("test");
        app.set_ready(true);
        finished(&mut app, "c1", "bash", "out", false);
        app.open_last_tool_output();
        app.handle_key(Key::Char('x'));
        assert!(app.input().is_empty(), "tidak boleh bocor ke composer");
    }
}

#[cfg(test)]
mod ctrl_c_semantics_tests {
    //! Test E2E PTY `m6_tui_connect` pernah gagal acak pada assertion
    //! "TUI harus memulihkan layar setelah Ctrl+C". Penyebabnya bukan PTY,
    //! melainkan semantik Ctrl+C yang **sengaja** berbeda: saat `busy` (turn
    //! berjalan) → Cancel dan TUI tetap terbuka; saat idle → Quit sehingga
    //! alternate screen dipulihkan. Teks jawaban dapat dirender *sebelum*
    //! `AgentFinished` membersihkan `busy`, jadi Ctrl+C yang dikirim tepat
    //! setelah jawaban terlihat bisa hanya membatalkan. Test di bawah mengunci
    //! kontrak itu supaya perilakunya tidak berubah tanpa sadar.

    use super::*;

    fn app_with_setup() -> App {
        let mut app = App::new("test");
        app.set_ready(true);
        app
    }

    #[test]
    fn ctrl_c_while_busy_cancels_and_keeps_tui_open() {
        let mut app = app_with_setup();
        app.begin_turn();
        assert!(app.busy());

        let action = app.handle_key(Key::CtrlC);
        assert_eq!(action, Action::Cancel);
        assert!(!app.should_quit(), "membatalkan tidak boleh menutup TUI");
    }

    #[test]
    fn ctrl_c_while_idle_quits() {
        let mut app = app_with_setup();
        assert!(!app.busy());
        let action = app.handle_key(Key::CtrlC);
        assert_eq!(action, Action::Quit);
        assert!(app.should_quit());
    }

    #[test]
    fn text_arriving_before_agent_finished_keeps_app_busy() {
        // Inilah jendela yang membuat test PTY flaky: jawaban sudah terlihat,
        // tetapi turn belum selesai.
        let mut app = app_with_setup();
        app.begin_turn();
        app.on_agent_event(AgentEvent::TextDelta {
            text: "banana".into(),
        });
        assert!(app.busy(), "teks datang sebelum AgentFinished");

        // Ctrl+C di sini = Cancel, bukan Quit — jadi PTY harus mengirim ulang.
        assert_eq!(app.handle_key(Key::CtrlC), Action::Cancel);
        assert!(!app.should_quit());

        // Setelah AgentFinished, Ctrl+C berikutnya benar-benar keluar.
        app.on_agent_event(AgentEvent::AgentFinished { steps: 1 });
        assert!(!app.busy());
        assert_eq!(app.handle_key(Key::CtrlC), Action::Quit);
        assert!(app.should_quit());
    }

    #[test]
    fn cancel_turn_returns_ui_to_idle() {
        let mut app = app_with_setup();
        app.begin_turn();
        app.cancel_turn();
        assert!(!app.busy());
        assert!(!app.should_quit());
    }

    // ---- autocomplete slash command -------------------------------------

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(Key::Char(c));
        }
    }

    fn items(app: &App) -> Vec<&'static str> {
        app.completion_view()
            .map(|v| v.items.clone())
            .unwrap_or_default()
    }

    #[test]
    fn typing_slash_opens_the_suggestion_list() {
        let mut app = App::new("test");
        type_str(&mut app, "/");
        let list = items(&app);
        assert!(!list.is_empty(), "daftar harus terbuka setelah '/'");
        assert!(list.contains(&"model"));
    }

    #[test]
    fn typing_filters_the_suggestion_list() {
        let mut app = App::new("test");
        type_str(&mut app, "/co");
        assert_eq!(items(&app), vec!["connect"]);
    }

    #[test]
    fn enter_accepts_the_selection_instead_of_submitting() {
        let mut app = App::new("test");
        type_str(&mut app, "/");
        // Pilih entri pertama, lalu Enter harus mengisi composer.
        assert_eq!(app.handle_key(Key::Enter), Action::None);
        assert_eq!(
            app.input(),
            "/model",
            "Enter memilih entri, bukan mengirim prompt"
        );
        assert_eq!(items(&app), Vec::<&str>::new(), "daftar ditutup");
    }

    #[test]
    fn tab_completes_without_running_the_command() {
        let mut app = App::new("test");
        type_str(&mut app, "/ver");
        assert_eq!(app.handle_key(Key::Tab), Action::None);
        // Command berargumen: harus ada spasi agar bisa langsung mengetik argumen.
        assert_eq!(app.input(), "/verify ");
        assert!(items(&app).is_empty());
    }

    #[test]
    fn arrows_move_the_selection_without_touching_history() {
        let mut app = App::new("test");
        app.load_history(vec![Message::user("earlier prompt")]);
        type_str(&mut app, "/");
        let first = app.completion_view().expect("terbuka").selected;
        assert_eq!(app.handle_key(Key::Down), Action::None);
        let second = app.completion_view().expect("terbuka").selected;
        assert_ne!(first, second, "Down memindahkan pilihan");
        // Composer tidak boleh berubah menjadi entri history.
        assert_eq!(app.input(), "/");
    }

    #[test]
    fn esc_closes_the_list_before_clearing_the_input() {
        let mut app = App::new("test");
        type_str(&mut app, "/mo");
        assert!(!items(&app).is_empty());
        // Esc pertama: hanya menutup daftar, teks tetap ada.
        assert_eq!(app.handle_key(Key::Esc), Action::None);
        assert!(items(&app).is_empty(), "daftar ditutup");
        assert_eq!(app.input(), "/mo", "teks tidak ikut terhapus");
        // Esc kedua: baru membersihkan composer.
        assert_eq!(app.handle_key(Key::Esc), Action::None);
        assert!(app.input().is_empty());
    }

    #[test]
    fn space_after_the_command_closes_the_list() {
        let mut app = App::new("test");
        type_str(&mut app, "/model ");
        assert!(items(&app).is_empty(), "spasi berarti mulai argumen");
    }

    #[test]
    fn completing_a_real_command_still_runs_it() {
        // Bukti tidak ada regresi: setelah autocomplete mengisi, Enter dengan
        // daftar tertutup menjalankan command seperti biasa.
        let mut app = App::new("test");
        type_str(&mut app, "/");
        app.handle_key(Key::Enter); // isi "/model"
        let action = app.handle_key(Key::Enter);
        assert!(
            matches!(action, Action::Command(Command::Model(_))),
            "dapat: {action:?}"
        );
    }

    #[test]
    fn ordinary_prompt_submits_even_with_slash_elsewhere() {
        let mut app = App::new("test");
        type_str(&mut app, "fix src/main.rs now");
        assert!(items(&app).is_empty());
        assert!(matches!(app.handle_key(Key::Enter), Action::Submit(_)));
    }

    #[test]
    fn backspace_closes_the_list_when_the_slash_is_gone() {
        let mut app = App::new("test");
        type_str(&mut app, "/");
        assert!(!items(&app).is_empty());
        app.handle_key(Key::Backspace);
        assert!(items(&app).is_empty(), "tanpa '/' daftar harus tertutup");
    }

    #[test]
    fn help_lists_every_registered_command() {
        let mut app = App::new("test");
        type_str(&mut app, "/help");
        app.handle_key(Key::Enter);
        let notes: Vec<String> = app
            .transcript()
            .iter()
            .filter_map(|line| match line {
                Line::Note(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        for cmd in crate::tui::slash::COMMANDS {
            assert!(
                notes.iter().any(|n| n.contains(&format!("/{}", cmd.name))),
                "/{} tidak muncul di bantuan: {notes:?}",
                cmd.name
            );
        }
    }
}
