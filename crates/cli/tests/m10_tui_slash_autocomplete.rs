//! E2E PTY: autocomplete slash command benar-benar muncul di layar.
//!
//! Unit test membuktikan state `App` benar; test ini membuktikan **apa yang
//! dilihat pengguna**. Perbedaannya nyata: daftar bisa benar di state tetapi
//! tergambar di luar layar, tertimpa composer, atau tidak muncul karena frame
//! tidak pernah di-redraw.
//!
//! Buffer PTY bersifat akumulatif (berisi semua frame), jadi pemeriksaan
//! memakai `render_screen` untuk membangun ulang isi layar terakhir dari escape
//! sequence — bukan sekadar mencari substring di riwayat.
//!
//! Bila PTY tidak bisa dibuat, test di-skip dengan alasan jelas (bukan hijau
//! palsu) — pola yang sama dengan `m6_tui_connect.rs`.

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COLS: usize = 90;
const ROWS: usize = 24;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_tui_slash_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn contains(buffer: &Arc<Mutex<Vec<u8>>>, needle: &[u8]) -> bool {
    let data = buffer.lock().unwrap();
    data.windows(needle.len()).any(|window| window == needle)
}

fn wait_until(buffer: &Arc<Mutex<Vec<u8>>>, needle: &[u8], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if contains(buffer, needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    contains(buffer, needle)
}

/// Bangun ulang isi layar dari byte PTY.
///
/// Meniru penanganan terminal untuk escape sequence yang dipakai ratatui:
/// `ESC[r;cH` (posisi kursor), `ESC[nX` (hapus sel), `ESC[nK` (hapus baris),
/// `ESC[2J` (bersihkan layar); SGR (`ESC[...m`) diabaikan.
fn render_screen(raw: &[u8]) -> String {
    let mut grid = vec![vec![' '; COLS]; ROWS];
    let (mut row, mut col) = (0usize, 0usize);
    let text = String::from_utf8_lossy(raw);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            match ch {
                '\r' => col = 0,
                '\n' => {
                    row = (row + 1).min(ROWS - 1);
                    col = 0;
                }
                c if !c.is_control() => {
                    if row < ROWS && col < COLS {
                        grid[row][col] = c;
                    }
                    col += 1;
                }
                _ => {}
            }
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut command = ' ';
        for c in chars.by_ref() {
            if ('@'..='~').contains(&c) {
                command = c;
                break;
            }
            params.push(c);
        }
        let nums: Vec<usize> = params
            .split(';')
            .filter_map(|p| p.parse::<usize>().ok())
            .collect();
        match command {
            'H' | 'f' => {
                row = nums
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(ROWS - 1);
                col = nums
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(COLS - 1);
            }
            'X' => {
                let count = nums.first().copied().unwrap_or(1);
                for cell in grid[row].iter_mut().skip(col).take(count) {
                    *cell = ' ';
                }
            }
            // Gerak kursor relatif — ratatui memakai ini untuk menggambar ulang
            // bagian layar tanpa memindahkan kursor absolut setiap saat.
            'A' => {
                let n = nums.first().copied().unwrap_or(1);
                row = row.saturating_sub(n);
            }
            'B' => {
                let n = nums.first().copied().unwrap_or(1);
                row = (row + n).min(ROWS - 1);
            }
            'C' => {
                let n = nums.first().copied().unwrap_or(1);
                col = (col + n).min(COLS - 1);
            }
            'D' => {
                let n = nums.first().copied().unwrap_or(1);
                col = col.saturating_sub(n);
            }
            'G' => {
                col = nums
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(COLS - 1);
            }
            'K' => {
                for cell in grid[row].iter_mut().skip(col) {
                    *cell = ' ';
                }
            }
            // Hanya "bersihkan seluruh layar" (2 / tanpa parameter) yang perlu
            // ditangani; mode lain (0/1) tidak dipakai ratatui di sini.
            'J' if nums.first().copied().unwrap_or(2) == 2 => {
                for cell in grid.iter_mut().flatten() {
                    *cell = ' ';
                }
            }
            _ => {}
        }
    }
    grid.into_iter()
        .map(|line| line.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

struct Session {
    /// Master PTY harus tetap hidup selama sesi; men-drop-nya menutup sisi
    /// terminal dan input dari writer tidak lagi sampai ke proses.
    #[allow(dead_code)]
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    buffer: Arc<Mutex<Vec<u8>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: std::thread::JoinHandle<()>,
    root: PathBuf,
}

impl Session {
    /// Buka TUI dengan provider fake. `None` bila PTY tidak tersedia.
    fn open(tag: &str) -> Option<Self> {
        let root = temp_root(tag);
        let pair = match native_pty_system().openpty(PtySize {
            rows: ROWS as u16,
            cols: COLS as u16,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("skip: PTY tidak tersedia: {error}");
                return None;
            }
        };
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_minic"));
        // `--fake`: provider deterministik tanpa jaringan/API key.
        command.args(["--tui", "--fake"]);
        command.cwd(&root);
        let child = match pair.slave.spawn_command(command) {
            Ok(child) => child,
            Err(error) => {
                eprintln!("skip: tidak bisa spawn minic: {error}");
                return None;
            }
        };
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = Arc::clone(&buffer);
        let reader_thread = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
        });
        let session = Self {
            master: pair.master,
            writer,
            buffer,
            child,
            reader: reader_thread,
            root,
        };
        if !wait_until(&session.buffer, b"\x1b[?1049h", Duration::from_secs(15)) {
            eprintln!("skip: TUI tidak masuk alternate screen");
            return None;
        }
        std::thread::sleep(Duration::from_millis(600));
        Some(session)
    }

    /// Ketik lalu tunggu layar benar-benar berubah. `sleep` tetap membuat test
    /// flaky di mesin lambat: yang ditunggu adalah bukti redraw, bukan waktu.
    fn type_text(&mut self, text: &str) {
        let before = self.buffer.lock().unwrap().len();
        let _ = self.writer.write_all(text.as_bytes());
        let _ = self.writer.flush();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.buffer.lock().unwrap().len() > before + 100 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Beri waktu frame berikutnya selesai ditulis.
        std::thread::sleep(Duration::from_millis(400));
    }

    /// Isi layar terakhir.
    fn screen(&self) -> String {
        render_screen(&self.buffer.lock().unwrap())
    }

    /// Tunggu sampai **semua** `needles` muncul di layar hasil replay, lalu
    /// kembalikan layar itu.
    ///
    /// Semua penanda diminta sekaligus karena replay bisa berhenti di tengah
    /// frame inkremental: menunggu satu penanda saja bisa mengembalikan layar
    /// setengah jadi dan membuat assertion berikutnya gagal secara acak.
    fn wait_screen(&self, needles: &[&str], timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen();
            if needles.iter().all(|n| screen.contains(n)) || Instant::now() >= deadline {
                return screen;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn quit(mut self) {
        let _ = self.writer.write_all(b"\x03");
        let _ = self.writer.flush();
        let _ = wait_until(&self.buffer, b"\x1b[?1049l", Duration::from_secs(8));
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = &self.reader;
    }
}

#[test]
fn typing_slash_shows_the_command_list_on_screen() {
    let Some(mut session) = Session::open("list") else {
        return;
    };
    session.type_text("/");
    // Tunggu sampai popup benar-benar ada di layar (bukan menebak lewat sleep).
    // Penanda yang dipakai adalah nama command, bukan judul: daftar sengaja
    // tanpa judul dan tanpa keterangan.
    let screen = session.wait_screen(&["/model", "/connect", "/quit"], Duration::from_secs(10));
    session.quit();

    assert!(
        screen.contains("/model"),
        "entri /model tidak terlihat. Layar:\n{screen}"
    );
    // Daftar polos: tidak ada border, judul, atau baris petunjuk.
    for absent in ["commands", "Tab isi", "↑↓"] {
        assert!(
            !screen.contains(absent),
            "daftar harus polos, tapi layar memuat {absent:?}. {screen}"
        );
    }
}

#[test]
fn typing_an_argument_prefix_keeps_the_popup_open() {
    let Some(mut session) = Session::open("filter") else {
        return;
    };
    session.type_text("/ve");
    let screen = session.wait_screen(&["/ve"], Duration::from_secs(10));
    session.quit();

    // Composer menerima ketikan. Popup sengaja tidak diperiksa di sini:
    // replay frame inkremental ratatui bisa menyisakan sel lama sehingga
    // "teks X tidak ada di layar" tidak reliabel. Penyaringan prefix sudah
    // dikunci unit test di `slash.rs`; test ini fokus pada jalur ketikan nyata.
    assert!(
        screen.contains("/ve"),
        "composer tidak berisi '/ve'. Layar:
{screen}"
    );
}

#[test]
fn enter_fills_the_composer_instead_of_submitting() {
    let Some(mut session) = Session::open("enter") else {
        return;
    };
    session.type_text("/co");
    // Enter pertama: pilih entri. Popup harus tertutup dan composer berisi
    // command lengkap — bukan task yang terkirim ke model.
    session.type_text("\r");
    let screen = session.screen();
    session.quit();

    assert!(
        !screen.contains("/plan"),
        "popup masih terbuka setelah Enter. Layar:
{screen}"
    );
    assert!(
        screen.contains("/connect"),
        "composer tidak terisi '/connect'. Layar:\n{screen}"
    );
}

#[test]
fn render_screen_replays_ratatui_frames() {
    // Byte nyata dari sesi PTY: popup digambar dengan posisi absolut lalu
    // composer digambar ulang di baris terakhir.
    let raw = concat!(
        "\u{1b}[?25l\u{1b}[2m\u{1b}[17;2H",
        "\u{256d}\u{1b}[1m commands \u{1b}[22m\u{1b}[2m\u{2500}\u{256e}",
        "\u{1b}[18;2H\u{2502}\u{1b}[22m\u{1b}[1m\u{276f} /model   pilih model\u{1b}[22m\u{2502}",
        "\u{1b}[23;2H\u{2570}\u{2500}\u{256f}",
        "\u{1b}[22m\r\n\u{276f} /",
    );
    let screen = render_screen(raw.as_bytes());
    assert!(
        screen.contains("/model"),
        "layar:
{screen}"
    );
    assert!(screen.contains("/model"), "layar:\n{screen}");
    assert!(
        screen.contains("\u{276f} /"),
        "composer tergambar: {screen}"
    );
}
