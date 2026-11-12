//! Fase E: `--max-output-tokens` benar-benar sampai ke request provider.
//!
//! Diuji lewat server HTTP lokal yang merekam body request. Ini bukti di
//! batas wire, bukan sekadar "flag ada di --help": nilai yang salah tempat
//! (mis. tertelan sebelum `RunOptions`) akan langsung ketahuan.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Server satu-tembakan: rekam body, balas SSE minimal yang langsung `stop`.
fn spawn_capture_server() -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let captured = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&captured);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Baca header + body (content-length).
            let mut body_start = None;
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if body_start.is_none()
                    && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    body_start = Some(pos + 4);
                }
                if let Some(start) = body_start {
                    let headers = String::from_utf8_lossy(&buf[..start]);
                    let want = headers
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= start + want {
                        break;
                    }
                }
            }
            if let Some(start) = body_start {
                *sink.lock().unwrap() = String::from_utf8_lossy(&buf[start..]).to_string();
            }
            let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                sse.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}/v1"), captured)
}

#[test]
fn max_output_tokens_reaches_the_provider_request_body() {
    let (base_url, captured) = spawn_capture_server();
    let root = std::env::temp_dir().join(format!(
        "minic_fasee_wire_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_minic"))
        .args([
            "--base-url",
            &base_url,
            "--model",
            "test-model",
            "--api-key",
            "k",
            "--max-output-tokens",
            "321",
            "--cwd",
        ])
        .arg(&root)
        .arg("say hi")
        .env_remove("MINIC_MAX_OUTPUT_TOKENS")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn minic");

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.wait();
    let body = captured.lock().unwrap().clone();
    let _ = std::fs::remove_dir_all(&root);

    assert!(!body.is_empty(), "server tidak menerima request");
    assert!(
        body.contains("\"max_tokens\":321"),
        "max_tokens tidak sampai ke provider. body: {body}"
    );
}

#[test]
fn absent_flag_sends_no_max_tokens() {
    let (base_url, captured) = spawn_capture_server();
    let root = std::env::temp_dir().join(format!(
        "minic_fasee_wire2_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_minic"))
        .args([
            "--base-url",
            &base_url,
            "--model",
            "test-model",
            "--api-key",
            "k",
            "--cwd",
        ])
        .arg(&root)
        .arg("say hi")
        .env_remove("MINIC_MAX_OUTPUT_TOKENS")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn minic");

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if !captured.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.wait();
    let body = captured.lock().unwrap().clone();
    let _ = std::fs::remove_dir_all(&root);

    assert!(!body.is_empty(), "server tidak menerima request");
    assert!(
        !body.contains("max_tokens"),
        "tanpa flag, tidak boleh ada max_tokens di body: {body}"
    );
}
