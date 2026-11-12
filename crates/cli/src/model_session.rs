//! Sesi model yang bisa diubah saat runtime.
//!
//! TUI boleh dibuka tanpa provider/model. Setelah pengguna menjalankan
//! `/connect` dan memilih model, sesi ini diperbarui dan turn berikutnya
//! memakai snapshot terbaru. Sub-agent membaca sesi yang sama, jadi mereka
//! tidak tertinggal pada provider/model lama.

use minic_core::provider::ModelProvider;
use std::sync::{Arc, RwLock};

/// Snapshot konsisten untuk satu turn: turn tidak boleh melihat provider dan
/// model dari generasi yang berbeda bila pengguna mengganti model di tengah
/// jalan (walaupun UI memblokir submit saat busy).
#[derive(Clone)]
pub struct ModelSnapshot {
    pub provider: Arc<dyn ModelProvider>,
    pub model: String,
    pub context_tokens: u32,
}

#[derive(Clone)]
pub struct ModelSession {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    provider: Option<Arc<dyn ModelProvider>>,
    model: String,
    context_tokens: u32,
}

impl ModelSession {
    pub fn new(
        provider: Option<Arc<dyn ModelProvider>>,
        model: impl Into<String>,
        context_tokens: u32,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                provider,
                model: model.into(),
                context_tokens: context_tokens.max(1),
            })),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        // Lock poison tidak boleh mematikan sesi TUI: data di dalamnya tetap
        // konsisten, jadi pulihkan guard-nya.
        self.inner.read().unwrap_or_else(|error| error.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn set_provider(&self, provider: Arc<dyn ModelProvider>) {
        self.write().provider = Some(provider);
    }

    /// Ganti model. `context_tokens` hanya ditimpa bila hasil deteksi tersedia.
    pub fn set_model(&self, model: impl Into<String>, context_tokens: Option<u32>) {
        let mut inner = self.write();
        inner.model = model.into();
        if let Some(tokens) = context_tokens {
            inner.context_tokens = tokens.max(1);
        }
    }

    pub fn provider_connected(&self) -> bool {
        self.read().provider.is_some()
    }

    pub fn model(&self) -> String {
        self.read().model.clone()
    }

    pub fn context_tokens(&self) -> u32 {
        self.read().context_tokens
    }

    pub fn configured(&self) -> bool {
        let inner = self.read();
        inner.provider.is_some() && !inner.model.trim().is_empty()
    }

    /// `None` bila provider atau model belum siap. Snapshot meng-clone `Arc`
    /// provider supaya turn tetap hidup walau sesi diganti.
    pub fn snapshot(&self) -> Option<ModelSnapshot> {
        let inner = self.read();
        let provider = inner.provider.clone()?;
        let model = inner.model.trim();
        if model.is_empty() {
            return None;
        }
        Some(ModelSnapshot {
            provider,
            model: inner.model.clone(),
            context_tokens: inner.context_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::FakeProvider;

    #[test]
    fn snapshot_requires_provider_and_model() {
        let empty = ModelSession::new(None, "", 128_000);
        assert!(!empty.configured());
        assert!(empty.snapshot().is_none());

        let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::default());
        let model_only = ModelSession::new(Some(provider.clone()), "", 128_000);
        assert!(!model_only.configured());
        assert!(model_only.snapshot().is_none());

        let ready = ModelSession::new(Some(provider), "fake", 64_000);
        assert!(ready.configured());
        let snapshot = ready.snapshot().expect("snapshot");
        assert_eq!(snapshot.model, "fake");
        assert_eq!(snapshot.context_tokens, 64_000);
    }

    #[test]
    fn model_and_context_can_be_updated() {
        let session = ModelSession::new(None, "", 128_000);
        session.set_model("gpt-4o-mini", Some(200_000));
        assert_eq!(session.model(), "gpt-4o-mini");
        assert_eq!(session.context_tokens(), 200_000);
        assert!(!session.configured(), "provider belum terhubung");

        session.set_model("gpt-4o-mini", None);
        assert_eq!(
            session.context_tokens(),
            200_000,
            "context tidak boleh reset"
        );
    }
}
