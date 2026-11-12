//! Regresi: sentinel `[DONE]` tidak boleh menimpa `finish_reason` sebenarnya.
//!
//! Bug yang dijaga: adapter OpenAI-compatible dulu selalu mengirim
//! `Finish(Stop)` saat melihat `[DONE]`, sehingga `finish_reason=length`
//! (respons terpotong) tertimpa menjadi "selesai normal" — dan gerbang
//! truncation di kernel tidak pernah aktif untuk provider nyata.
//!
//! Diuji di batas wire: server lokal mengirim `finish_reason: "length"`
//! lalu `[DONE]`, dan klien harus tetap melaporkan `Length`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use futures_util::StreamExt;
use minic::provider::openai_compat::OpenAiCompat;
use minic_core::provider::{FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::types::Message;

/// Tulis satu event SSE.
fn write_event(out: &mut String, value: &serde_json::Value) {
    out.push_str("data: ");
    out.push_str(&value.to_string());
    out.push_str("\n\n");
}

fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Server yang menjawab setiap request chat dengan `finish_reason` tertentu
/// lalu `[DONE]`; model-list dibalas JSON biasa.
fn spawn(finish_reason: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
            // Baca header saja: body tidak diperlukan untuk balasan.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut headers_done = false;
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if !headers_done && buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    headers_done = true;
                    // Body chunked: berhenti begitu ada tanda akhir.
                    if buf.windows(5).any(|w| w == b"0\r\n\r\n") {
                        break;
                    }
                } else if headers_done && buf.windows(5).any(|w| w == b"0\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf).to_ascii_lowercase();
            if head.starts_with("get ") {
                respond(
                    &mut stream,
                    "application/json",
                    "{\"data\":[{\"id\":\"m\",\"context_length\":128000}]}",
                );
                continue;
            }
            let mut sse = String::new();
            write_event(
                &mut sse,
                &serde_json::json!({"choices":[{"delta":{"content":"partial"},"finish_reason":finish_reason}]}),
            );
            sse.push_str("data: [DONE]\n\n");
            respond(&mut stream, "text/event-stream", &sse);
        }
    });
    format!("http://{addr}/v1")
}

async fn collect_finish(base_url: &str) -> FinishReason {
    let provider = OpenAiCompat::new(base_url, "k");
    let request = StreamRequest {
        model: "m".to_string(),
        system: None,
        messages: vec![Message::user("hi")],
        tools: Vec::new(),
        max_tokens: None,
    };
    let mut stream = provider.stream(request);
    let mut finish = FinishReason::Unknown;
    while let Some(item) = stream.next().await {
        if let Ok(ProviderEvent::Finish(reason)) = item {
            finish = reason;
        }
    }
    finish
}

#[tokio::test]
async fn done_sentinel_preserves_length_finish_reason() {
    let url = spawn("length");
    assert_eq!(
        collect_finish(&url).await,
        FinishReason::Length,
        "[DONE] tidak boleh menimpa finish_reason=length menjadi Stop"
    );
}

#[tokio::test]
async fn done_sentinel_preserves_tool_calls_finish_reason() {
    let url = spawn("tool_calls");
    assert_eq!(collect_finish(&url).await, FinishReason::ToolCalls);
}

#[tokio::test]
async fn stop_finish_reason_is_passed_through() {
    let url = spawn("stop");
    assert_eq!(collect_finish(&url).await, FinishReason::Stop);
}
