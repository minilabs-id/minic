//! Adapter OpenAI-compatible (`POST {base}/chat/completions`, `stream: true`).
//!
//! Streaming diwujudkan lewat task yang mengirim event ke channel, lalu
//! channel-nya di-`zip` jadi `Stream`. Ini menghindari kerumitan lifetime
//! `async` generator dan menjaga stream tetap `'static + Send`.

use crate::provider::sse::SseDecoder;
use futures_util::StreamExt;
use minic_core::error::AgentError;
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use minic_core::types::{Message, Role};
use serde_json::json;
use tokio::sync::mpsc;

pub struct OpenAiCompat {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

/// Satu entri `GET {base}/models` yang dipakai picker TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub context_length: Option<u32>,
}

impl OpenAiCompat {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        // Timeout wajib: tanpa ini, endpoint yang menggantung (DNS/read hang)
        // akan menggantung agent selamanya. `connect_timeout` mencegah hang
        // koneksi; `timeout` total menjaga stream yang tidak pernah selesai.
        // Angka ini longgar supaya stream panjang (banyak tool call) aman.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    /// Daftar model dari `GET {base}/models`.
    ///
    /// Error dikembalikan sebagai teks yang aman ditampilkan di TUI (tanpa
    /// body penuh); pemanggil bisa menawarkan input model manual.
    pub async fn list_models(&self) -> Result<Vec<ModelEntry>, String> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(20))
            .header("authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|error| format!("network: {error}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "http {}: {}",
                status.as_u16(),
                truncate(&body, 300)
            ));
        }
        let value: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| format!("invalid JSON: {error}"))?;
        let mut models = parse_models(&value);
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Ambil `context_length` model dari `GET {base}/models` (best-effort).
    /// Router OpenAI-compatible biasanya menyertakan `context_length` per
    /// model. Gagal → `None` (pemanggil memakai default).
    pub async fn fetch_context_window(&self, model: &str) -> Option<u32> {
        self.list_models()
            .await
            .ok()?
            .into_iter()
            .find(|entry| entry.id == model)
            .and_then(|entry| entry.context_length)
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

impl ModelProvider for OpenAiCompat {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let (tx, rx) = mpsc::channel::<Result<ProviderEvent, AgentError>>(64);
        let client = self.client.clone();
        let url = self.endpoint();
        let key = self.api_key.clone();
        let body = build_body(&req);

        tokio::spawn(async move {
            if let Err(error) = pump(client, url, key, body, &tx).await {
                let _ = tx.send(Err(error)).await;
            }
        });

        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

type EventTx = mpsc::Sender<Result<ProviderEvent, AgentError>>;

/// Retry eksponensial untuk status sementara (429/5xx) — banyak router/model
/// sempat 429 atau 502 di tengah beban. Batas 3 percobaan; hormati
/// `Retry-After` bila disediakan server. 4xx lain langsung gagal (jangan
/// retry: 401/403 tidak akan berubah).
const MAX_RETRIES: u32 = 3;
const BASE_BACKOFF_MS: u64 = 500;

fn is_retryable(status: reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    code == 429 || (500..=504).contains(&code)
}

async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response, AgentError> {
    let mut attempt = 0u32;
    loop {
        let response = client
            .post(url)
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| AgentError::Network(e.to_string()))?;

        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        if !is_retryable(status) || attempt >= MAX_RETRIES {
            let text = response.text().await.unwrap_or_default();
            return Err(AgentError::Provider(format!(
                "http {}: {}",
                status.as_u16(),
                truncate(&text, 500)
            )));
        }

        // Backoff: hormati Retry-After (detik), kalau ada; selain itu eksponensial.
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let wait_ms = retry_after
            .map(|secs| secs * 1000)
            .unwrap_or_else(|| BASE_BACKOFF_MS * 2u64.pow(attempt));
        attempt += 1;
        tracing::warn!(
            status = status.as_u16(),
            attempt,
            wait_ms,
            "provider request retryable; backing off"
        );
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
    }
}

async fn pump(
    client: reqwest::Client,
    url: String,
    key: String,
    body: serde_json::Value,
    tx: &EventTx,
) -> Result<(), AgentError> {
    let response = send_with_retry(&client, &url, &key, &body).await?;

    let mut byte_stream = response.bytes_stream();
    let mut decoder = SseDecoder::new();

    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk.map_err(|e| AgentError::Network(e.to_string()))?;
        for data in decoder.push(&chunk) {
            if data.trim() == "[DONE]" {
                let _ = tx.send(Ok(ProviderEvent::Finish(FinishReason::Stop))).await;
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
                emit_choices(&value, tx).await;
            }
        }
    }

    Ok(())
}

async fn emit_choices(value: &serde_json::Value, tx: &EventTx) {
    let Some(choices) = value.get("choices").and_then(|c| c.as_array()) else {
        return;
    };
    for choice in choices {
        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(|x| x.as_str())
                && !text.is_empty()
            {
                let _ = tx
                    .send(Ok(ProviderEvent::TextDelta(text.to_string())))
                    .await;
            }
            if let Some(reasoning) = delta.get("reasoning_content").and_then(|x| x.as_str())
                && !reasoning.is_empty()
            {
                let _ = tx
                    .send(Ok(ProviderEvent::ReasoningDelta(reasoning.to_string())))
                    .await;
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                for call in calls {
                    let index = call.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    let id = call.get("id").and_then(|x| x.as_str()).map(str::to_string);
                    let name = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    let args = call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let _ = tx
                        .send(Ok(ProviderEvent::ToolCallDelta {
                            index,
                            id,
                            name,
                            args_fragment: args,
                        }))
                        .await;
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            let finish = match reason {
                "stop" => FinishReason::Stop,
                "tool_calls" => FinishReason::ToolCalls,
                "length" => FinishReason::Length,
                "content_filter" => FinishReason::ContentFilter,
                _ => FinishReason::Unknown,
            };
            let _ = tx.send(Ok(ProviderEvent::Finish(finish))).await;
        }
    }
}

fn build_body(req: &StreamRequest) -> serde_json::Value {
    let mut messages = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    messages.extend(req.messages.iter().map(to_wire_message));

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
    });
    if !req.tools.is_empty() {
        body["tools"] = json!(req.tools);
    }
    if let Some(max_tokens) = req.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    body
}

fn to_wire_message(message: &Message) -> serde_json::Value {
    match message.role {
        Role::System => json!({ "role": "system", "content": message.text() }),
        Role::User => json!({ "role": "user", "content": message.text() }),
        Role::Assistant => {
            if message.tool_calls.is_empty() {
                json!({ "role": "assistant", "content": message.text() })
            } else {
                let calls: Vec<serde_json::Value> = message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                let content = if message.text().is_empty() {
                    serde_json::Value::Null
                } else {
                    json!(message.text())
                };
                json!({ "role": "assistant", "content": content, "tool_calls": calls })
            }
        }
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
            "content": message.text(),
        }),
    }
}

fn parse_models(value: &serde_json::Value) -> Vec<ModelEntry> {
    value
        .get("data")
        .and_then(|data| data.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("id")?.as_str()?.to_string();
                    let context_length = entry
                        .get("context_length")
                        .and_then(|v| v.as_u64())
                        .and_then(|v| u32::try_from(v).ok())
                        .or_else(|| {
                            entry
                                .get("context_window")
                                .and_then(|v| v.as_u64())
                                .and_then(|v| u32::try_from(v).ok())
                        });
                    Some(ModelEntry { id, context_length })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{is_retryable, parse_models};
    use reqwest::StatusCode;

    #[test]
    fn retries_only_transient_statuses() {
        // 429/5xx sementara → retry; 4xx lain (auth/validasi) tidak akan
        // berubah, jadi jangan buang waktu (dan kuota) untuk retry.
        for code in [429, 500, 502, 503, 504] {
            assert!(is_retryable(StatusCode::from_u16(code).unwrap()), "{code}");
        }
        for code in [400, 401, 403, 404, 406, 422] {
            assert!(!is_retryable(StatusCode::from_u16(code).unwrap()), "{code}");
        }
    }

    #[test]
    fn parses_model_list_and_context_length() {
        let body = serde_json::json!({
            "data": [
                { "id": "alpha", "context_length": 128000 },
                { "id": "beta", "context_window": 64000 },
                { "id": "no-context" },
                { "object": "model" }
            ]
        });
        let models = parse_models(&body);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "alpha");
        assert_eq!(models[0].context_length, Some(128_000));
        assert_eq!(models[1].context_length, Some(64_000));
        assert_eq!(models[2].context_length, None);
    }

    #[test]
    fn malformed_model_list_is_empty() {
        assert!(parse_models(&serde_json::json!({ "data": "nope" })).is_empty());
        assert!(parse_models(&serde_json::json!({})).is_empty());
    }
}
