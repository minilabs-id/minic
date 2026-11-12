//! Fase B (end-to-end): verifikasi nyata menyuntikkan error compiler ke model.
//!
//! Test lain memakai `bash` palsu. Yang ini menjalankan `cargo check`
//! sungguhan pada crate rusak dan memeriksa bahwa pesan error compiler
//! benar-benar muncul di request model berikutnya — bukti bahwa loop
//! verifikasi menutup siklus, bukan hanya mencetak ke stderr.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Tulis satu event SSE: `data: <json>` lalu baris kosong.
fn write_event(out: &mut String, value: &serde_json::Value) {
    out.push_str("data: ");
    out.push_str(&value.to_string());
    out.push_str("\n\n");
}

/// Apakah seluruh body chunked sudah diterima (chunk akhir `0\r\n\r\n`)?
fn chunked_complete(raw: &[u8]) -> bool {
    raw.windows(5).any(|w| w == b"0\r\n\r\n")
}

/// Gabungkan body `Transfer-Encoding: chunked` menjadi string.
fn dechunk(raw: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let Some(nl) = raw[i..].windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_field = String::from_utf8_lossy(&raw[i..i + nl]);
        let Ok(size) = usize::from_str_radix(size_field.trim(), 16) else {
            break;
        };
        i += nl + 2;
        if size == 0 {
            break;
        }
        let end = (i + size).min(raw.len());
        out.extend_from_slice(&raw[i..end]);
        i = end + 2; // lewati CRLF setelah data
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Baca satu request HTTP: kembalikan (request-line, body). Mendukung
/// `Content-Length` maupun `Transfer-Encoding: chunked` (reqwest memakai
/// chunked saat streaming, jadi hanya membaca Content-Length menghasilkan
/// body kosong).
fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut body_start = None;
    let mut is_chunked = false;
    let mut want_len = 0usize;
    let mut request_line = String::new();
    loop {
        if let Some(start) = body_start {
            if is_chunked {
                if chunked_complete(&buf[start..]) {
                    break;
                }
            } else if buf.len() >= start + want_len {
                break;
            }
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if body_start.is_none()
            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            request_line = headers.lines().next().unwrap_or_default().to_string();
            is_chunked = headers.contains("transfer-encoding: chunked");
            want_len = headers
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            body_start = Some(pos + 4);
        }
    }
    let start = body_start?;
    let raw = &buf[start..];
    let body = if is_chunked {
        dechunk(raw)
    } else {
        String::from_utf8_lossy(raw).to_string()
    };
    Some((request_line, body))
}

fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Server penangkap: merekam body tiap request chat, membalas tool call pada
/// request chat pertama dan teks final pada request berikutnya.
fn spawn_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&bodies);
    std::thread::spawn(move || {
        let mut chat_turns = 0usize;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            let Some((request_line, body)) = read_request(&mut stream) else {
                continue;
            };

            // `GET /v1/models` dipakai untuk penemuan model; balas JSON biasa,
            // bukan SSE chat (kalau tertukar, tool call pertama hilang).
            if request_line.starts_with("get ") {
                respond(
                    &mut stream,
                    "application/json",
                    "{\"data\":[{\"id\":\"test-model\",\"context_length\":128000}]}",
                );
                continue;
            }

            sink.lock().unwrap().push(body);

            let mut sse = String::new();
            if chat_turns == 0 {
                // Bentuk yang dipakai provider nyata: `finish_reason` datang
                // pada event terpisah dari delta tool call.
                let args = serde_json::json!({
                    "path": "src/main.rs",
                    "content": "fn main() { let x: i32 = \"salah\"; }\n"
                })
                .to_string();
                write_event(
                    &mut sse,
                    &serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write_file","arguments":args}}]},"finish_reason":"tool_calls"}]}),
                );
            } else {
                write_event(
                    &mut sse,
                    &serde_json::json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
                );
            }
            sse.push_str("data: [DONE]\n\n");
            chat_turns += 1;
            respond(&mut stream, "text/event-stream", &sse);
        }
    });
    (format!("http://{addr}/v1"), bodies)
}

fn temp_crate() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_faseb_e2e_{nanos}"));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"vp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    root
}

#[test]
fn compiler_error_from_verify_reaches_the_model() {
    let (base_url, bodies) = spawn_server();
    let root = temp_crate();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_minic"))
        .args([
            "--base-url",
            &base_url,
            "--model",
            "test-model",
            "--api-key",
            "k",
            "--verify",
            "cargo check --offline --quiet",
            "--cwd",
        ])
        .arg(&root)
        .arg("break the code")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn minic");

    // Tunggu sampai model dipanggil lagi setelah verifikasi.
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        if bodies.lock().unwrap().len() >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let captured = bodies.lock().unwrap().clone();
    let file = std::fs::read_to_string(root.join("src/main.rs")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        captured.len() >= 2,
        "model harus dipanggil lagi setelah verifikasi; dapat {} request chat",
        captured.len()
    );
    assert!(
        file.contains("salah"),
        "write_file harus benar-benar menulis berkas; isi: {file}"
    );

    let second = &captured[1];
    // Verifikasi menyuntikkan hasil `cargo check` ke history. Error compiler
    // nyata (kode E0308) harus terlihat oleh model pada request berikutnya.
    assert!(
        second.contains("verification FAILED") || second.contains("E0308"),
        "hasil verifikasi harus sampai ke model. body: {}",
        &second[..second.len().min(2000)]
    );
}
