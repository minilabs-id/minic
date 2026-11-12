//! E2E PTY: TUI dibuka tanpa model, lalu provider dihubungkan dari dalam.
//!
//! Alur yang diverifikasi (tanpa jaringan keluar, memakai server HTTP lokal):
//! 1. `minic --tui --base-url <local>` masuk alternate screen walau tidak ada
//!    model/API key di env/config.
//! 2. `/connect` membuka form; isi API key lalu Enter.
//! 3. `/model` (otomatis setelah connect) menampilkan picker dari
//!    `GET /v1/models`; Enter memilih model pertama.
//! 4. Task dijalankan dan jawaban provider muncul.
//!
//! Bila PTY tidak bisa dibuat/di-spawn, test di-skip dengan alasan jelas
//! (bukan hijau palsu) — pola yang sama dengan `m6_tui_pty.rs`.

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_tui_connect_{tag}_{nanos}"));
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

fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Server OpenAI-compatible minimal: `/v1/models` + streaming chat.
///
/// Thread-nya sengaja tidak di-join; ia hanya hidup selama test berjalan dan
/// tidak menyentuh state global.
fn spawn_stub_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let Ok(mut stream) = stream.try_clone() else {
                continue;
            };
            let mut header = Vec::new();
            let mut chunk = [0u8; 1024];
            // Baca header sampai akhir; simpan batasnya untuk memisahkan body.
            while !header.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => header.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let head = String::from_utf8_lossy(&header).to_ascii_lowercase();
            // Kuras body berdasarkan Content-Length agar socket tidak di-close
            // dengan data belum terbaca (mencegah RST yang bisa merusak respons).
            let content_length = head
                .lines()
                .find(|line| line.starts_with("content-length:"))
                .and_then(|line| line.split(':').nth(1))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let mut remaining = content_length;
            while remaining > 0 {
                let mut drain = [0u8; 1024];
                match stream.read(&mut drain) {
                    Ok(0) => break,
                    Ok(n) => remaining -= n.min(remaining),
                    Err(_) => break,
                }
            }
            if head.starts_with("get ") && head.contains("/models") {
                respond(
                    &mut stream,
                    "application/json",
                    r#"{"data":[{"id":"stub-model","context_length":32000}]}"#,
                );
            } else if head.starts_with("post ") && head.contains("/chat/completions") {
                // Dua event: delta teks lalu finish, ditutup [DONE].
                respond(
                    &mut stream,
                    "text/event-stream",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"banana\"}}]}\n\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n",
                );
            } else {
                respond(&mut stream, "application/json", r#"{"error":"not found"}"#);
            }
        }
    });
    format!("http://{addr}/v1")
}

#[test]
fn tui_connects_and_picks_model_without_startup_model() {
    let root = temp_root("connect");
    let base_url = spawn_stub_server();

    let pty_system = native_pty_system();
    let Ok(pair) = pty_system.openpty(PtySize {
        rows: 32,
        cols: 110,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        eprintln!("skip: openpty failed on this host");
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_minic"));
    command.args(["--tui", "--base-url", &base_url, "--cwd"]);
    command.arg(&root);
    command.cwd(&root);
    // Pastikan test tidak mewarisi model/key dari lingkungan pemanggil.
    for key in [
        "MINIC_MODEL",
        "MINIC_API_KEY",
        "OPENAI_API_KEY",
        "MINIC_BASE_URL",
        "MINIC_CONTEXT_TOKENS",
    ] {
        command.env_remove(key);
    }

    let Ok(mut child) = pair.slave.spawn_command(command) else {
        eprintln!("skip: spawn_command failed on this host");
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));

    let reader_buffer = buffer.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => reader_buffer.lock().unwrap().extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
    });

    let entered = wait_until(&buffer, b"\x1b[?1049h", Duration::from_secs(15));
    // Tanpa model/key, TUI harus tetap terbuka dan menawarkan /connect.
    let hinted = wait_until(&buffer, b"/connect", Duration::from_secs(15));

    // Buka form connect, isi API key, submit. Base URL sudah terisi dari CLI.
    let _ = writer.write_all(b"/connect\r");
    let _ = writer.flush();
    let form = wait_until(&buffer, b"connect provider", Duration::from_secs(10));
    let _ = writer.write_all(b"\tstub-key\r");
    let _ = writer.flush();

    // Picker otomatis terbuka setelah provider terpasang.
    let listed = wait_until(&buffer, b"stub-model", Duration::from_secs(20));
    let _ = writer.write_all(b"\r");
    let _ = writer.flush();
    let picked = wait_until(&buffer, b"model: stub-model", Duration::from_secs(10));

    // Turn nyata lewat provider yang baru dipilih.
    let _ = writer.write_all(b"hi\r");
    let _ = writer.flush();
    let answered = wait_until(&buffer, b"banana", Duration::from_secs(20));

    // Ctrl+C saat **streaming** membatalkan turn (tetap di TUI); saat **idle**
    // baru keluar. Teks jawaban bisa tampil sebelum `AgentFinished` membersihkan
    // status sibuk, jadi satu Ctrl+C bisa jatuh di jendela itu dan hanya
    // membatalkan — layar tidak dipulihkan dan test gagal secara acak.
    // Karena itu: kirim Ctrl+C berulang sampai alternate screen benar-benar
    // ditinggalkan, bukan menebak dengan `sleep` tetap.
    let restore_deadline = Instant::now() + Duration::from_secs(20);
    let mut restored = false;
    while Instant::now() < restore_deadline {
        let _ = writer.write_all(&[0x03]);
        let _ = writer.flush();
        if wait_until(&buffer, b"\x1b[?1049l", Duration::from_millis(500)) {
            restored = true;
            break;
        }
    }
    if !restored {
        // Kesempatan terakhir: proses mungkin sudah keluar sendiri.
        restored = wait_until(&buffer, b"\x1b[?1049l", Duration::from_secs(5));
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                break;
            }
        }
    }

    drop(writer);
    drop(pair.master);
    let _ = reader_thread.join();
    let _ = std::fs::remove_dir_all(&root);

    assert!(entered, "TUI harus masuk alternate screen tanpa model");
    assert!(hinted, "TUI harus menampilkan petunjuk /connect");
    assert!(form, "/connect harus membuka form provider");
    assert!(listed, "/model harus menampilkan model dari /v1/models");
    assert!(picked, "memilih model harus menutup picker dan mencatatnya");
    assert!(answered, "turn harus memakai model yang dipilih dari TUI");
    assert!(restored, "TUI harus memulihkan layar setelah Ctrl+C");
}
