//! Language server palsu untuk test `lsp_diagnostics`.
//!
//! Framing `Content-Length`. Menjawab `initialize`/`shutdown`, dan saat
//! `textDocument/didOpen` mengirim satu `publishDiagnostics`.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();

    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            // Parse header case-insensitif (spec LSP): server nyata bisa
            // mengirim `content-length:` huruf kecil.
            if let Some((name, rest)) = trimmed.split_once(':')
                && name.eq_ignore_ascii_case("Content-Length")
            {
                content_length = rest.trim().parse().ok();
            }
        }
        let Some(length) = content_length else { return };
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        handle(&message, &mut stdout);
    }
}

fn send(out: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_default();
    // Sengaja kirim nama header huruf kecil untuk membuktikan client
    // menerima header case-insensitif.
    let _ = write!(out, "content-length: {}\r\n\r\n", body.len());
    let _ = out.write_all(&body);
    let _ = out.flush();
}

fn handle(message: &Value, out: &mut impl Write) {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => {
            if let Some(id) = id {
                send(
                    out,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": { "capabilities": {} } }),
                );
            }
        }
        "textDocument/didOpen" => {
            let uri = message["params"]["textDocument"]["uri"]
                .as_str()
                .unwrap_or("");
            send(
                out,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": uri,
                        "diagnostics": [{
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 1 }
                            },
                            "severity": 1,
                            "message": "fake diagnostic"
                        }]
                    }
                }),
            );
        }
        "shutdown" => {
            if let Some(id) = id {
                send(
                    out,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }),
                );
            }
        }
        "exit" => std::process::exit(0),
        _ => {
            if let Some(id) = id {
                send(
                    out,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }),
                );
            }
        }
    }
}
