//! Klien LSP minimal: spawn language server, framing `Content-Length`,
//! `initialize`, `textDocument/didOpen`, tunggu `publishDiagnostics`.
//!
//! Koneksi bersifat per-panggilan (spawn → diagnostics → shutdown). Sederhana
//! dan tanpa state global; cukup untuk tool `lsp_diagnostics`.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct LspConfig {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerSpec>,
}

pub fn config_path(root: &Path) -> PathBuf {
    crate::config::minic_dir(root).join("lsp.json")
}

pub fn load_config(root: &Path) -> LspConfig {
    match std::fs::read_to_string(config_path(root)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => LspConfig::default(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u64,
    pub severity: &'static str,
    pub message: String,
}

struct LspProc {
    _child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl LspProc {
    async fn spawn(spec: &ServerSpec) -> Result<Self, String> {
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn '{}': {e}", spec.command))?;
        let stdin = child.stdin.take().ok_or("no stdin pipe")?;
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;
        Ok(Self {
            _child: child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        })
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(message).map_err(|e| e.to_string())?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.stdin
            .write_all(&body)
            .await
            .map_err(|e| e.to_string())?;
        self.stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("lsp '{method}' timed out"));
            }
            let frame = tokio::time::timeout(remaining, read_frame(&mut self.reader))
                .await
                .map_err(|_| format!("lsp '{method}' timed out"))?;
            let Some(message) = frame else {
                return Err("lsp server closed".to_string());
            };
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(error.to_string());
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }
}

async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let trimmed = line.trim_end();
        // Header LSP bersifat case-insensitive (spec); jangan hanya menerima
        // persis "Content-Length:" — server nyata bisa menulis huruf kecil.
        if let Some((name, rest)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            content_length = rest.trim().parse().ok();
        }
    }
    let length = content_length?;
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).await.is_err() {
        return None;
    }
    serde_json::from_slice(&body).ok()
}

fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        format!("file:///{normalized}")
    } else {
        format!("file://{normalized}")
    }
}

/// Ambil diagnostik untuk satu berkas menggunakan server sesuai ekstensi.
pub async fn diagnostics(root: &Path, path: &Path) -> Result<Vec<Diagnostic>, String> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| "file has no extension".to_string())?
        .to_string();

    let config = load_config(root);
    let spec = config
        .servers
        .get(&extension)
        .ok_or_else(|| format!("no LSP server configured for '.{extension}'"))?;

    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let uri = file_uri(path);
    let root_uri = file_uri(root);

    let mut proc = LspProc::spawn(spec).await?;
    proc.request(
        "initialize",
        json!({
            "processId": Value::Null,
            "rootUri": root_uri,
            "capabilities": {}
        }),
    )
    .await?;
    proc.notify("initialized", json!({})).await?;
    proc.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": extension,
                "version": 1,
                "text": text
            }
        }),
    )
    .await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut diagnostics = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = tokio::time::timeout(remaining, read_frame(&mut proc.reader)).await;
        let Ok(Some(message)) = frame else { break };
        if message.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics")
        {
            continue;
        }
        let params = &message["params"];
        if params.get("uri").and_then(Value::as_str) != Some(uri.as_str()) {
            continue;
        }
        if let Some(items) = params.get("diagnostics").and_then(Value::as_array) {
            for item in items {
                diagnostics.push(parse_diagnostic(item));
            }
        }
        break;
    }

    let _ = proc.request("shutdown", Value::Null).await;
    let _ = proc.notify("exit", Value::Null).await;
    Ok(diagnostics)
}

fn parse_diagnostic(item: &Value) -> Diagnostic {
    let line = item["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
    let severity = match item.get("severity").and_then(Value::as_u64) {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "unknown",
    };
    let message = item
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Diagnostic {
        line,
        severity,
        message,
    }
}
