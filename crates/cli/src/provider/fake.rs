//! Provider deterministik untuk demo & test (tanpa jaringan).
//!
//! Putaran pertama: minta `read_file("Cargo.toml")`.
//! Putaran berikutnya: balas teks ringkas lalu selesai.

use minic_core::error::AgentError;
use minic_core::provider::{BoxStream, FinishReason, ModelProvider, ProviderEvent, StreamRequest};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

#[derive(Default)]
pub struct FakeProvider {
    calls: AtomicUsize,
}

impl ModelProvider for FakeProvider {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel(16);

        // Kontrak: model wajib diisi host. Fake provider ikut menegakkannya
        // supaya bug "model kosong" tidak lolos diam-diam di test TUI.
        if req.model.trim().is_empty() {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta(
                "ERROR: empty model\n".to_string(),
            )));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
            drop(tx);
            return Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
        }

        if call == 0 && !req.tools.is_empty() {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta(
                "Let me look at the project.\n".to_string(),
            )));
            let _ = tx.try_send(Ok(ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_demo_0".to_string()),
                name: Some("read_file".to_string()),
                args_fragment: "{\"path\":\"Cargo.toml\"}".to_string(),
            }));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::ToolCalls)));
        } else {
            let _ = tx.try_send(Ok(ProviderEvent::TextDelta(format!(
                "Done. (fake provider; {} messages in context)\n",
                req.messages.len()
            ))));
            let _ = tx.try_send(Ok(ProviderEvent::Finish(FinishReason::Stop)));
        }

        drop(tx);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}
