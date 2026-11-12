//! Klien MCP satu server (stdio, JSON-RPC satu objek per baris).

use crate::mcp::config::ServerSpec;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

pub struct McpClient {
    name: String,
    /// Anak proses; dibunuh otomatis saat drop (`kill_on_drop`).
    _child: Child,
    stdin: AsyncMutex<ChildStdin>,
    next_id: AtomicU64,
    pending: std::sync::Arc<Pending>,
    _reader: tokio::task::JoinHandle<()>,
}

impl McpClient {
    pub async fn connect(name: &str, spec: &ServerSpec) -> Result<Self, String> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("spawn '{}': {e}", spec.command))?;
        let stdin = child.stdin.take().ok_or("no stdin pipe")?;
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;

        let pending: std::sync::Arc<Pending> = std::sync::Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let Some(id) = value.get("id").and_then(Value::as_u64) else {
                    continue; // notifikasi server → diabaikan
                };
                let sender = reader_pending.lock().unwrap().remove(&id);
                if let Some(sender) = sender {
                    if let Some(error) = value.get("error") {
                        let _ = sender.send(Err(format!("mcp error: {error}")));
                    } else {
                        let _ =
                            sender.send(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
                    }
                }
            }
        });

        let client = Self {
            name: name.to_string(),
            _child: child,
            stdin: AsyncMutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending,
            _reader: reader,
        };
        client.initialize().await?;
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    async fn initialize(&self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "minic", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);

        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_line(&message).await?;

        match tokio::time::timeout(REQUEST_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("mcp: response channel closed".to_string()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("mcp: '{method}' timed out"))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&message).await
    }

    async fn write_line(&self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    pub async fn list_tools(&self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Kembalikan `(teks, is_error)`.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<(String, bool), String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut text = String::new();
        if let Some(content) = result.get("content").and_then(Value::as_array) {
            for part in content {
                if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(chunk);
                }
            }
        }
        if text.is_empty() {
            text = result.to_string();
        }
        Ok((text, is_error))
    }
}
