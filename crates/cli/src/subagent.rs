//! Sub-agent: pool berbatas untuk mendelegasikan tugas dengan konteks terpisah.
//!
//! - Maksimum **10** sub-agent hidup bersamaan (`MAX_SUBAGENTS`), bisa
//!   diturunkan lewat `max_subagents` di `.minic/config.toml`.
//! - Sub-agent **read-only**: hanya `read_file`, `grep`, `glob`.
//! - **Tanpa nesting**: `delegate_task` sengaja TIDAK didaftarkan ke registry
//!   sub-agent, sehingga sub-agent tidak bisa merekrut sub-agent lagi
//!   (mencegah rekursi dan pelanggaran batas).

use crate::model_session::ModelSession;
use crate::policy::SimplePolicy;
use crate::tools;
use minic_core::agent::{RunOptions, run_agent};
use minic_core::history::ContextStore;
use minic_core::permission::PermissionMode;
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::{Message, Role};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;

pub const MAX_SUBAGENTS: usize = 10;

pub struct SubAgentPool {
    /// Sesi yang sama dengan TUI: sub-agent ikut model/provider yang sedang
    /// aktif, bukan snapshot saat startup.
    session: Arc<ModelSession>,
    system: Option<String>,
    max_steps: u32,
    root: PathBuf,
    semaphore: Arc<Semaphore>,
    /// Batas efektif (≤ `MAX_SUBAGENTS`). Disimpan agar error & schema bisa
    /// menyebut angka yang benar-benar berlaku, bukan konstanta global.
    capacity: usize,
}

impl SubAgentPool {
    pub fn new(
        session: Arc<ModelSession>,
        system: Option<String>,
        max_steps: u32,
        root: PathBuf,
    ) -> Self {
        Self::with_capacity(session, system, max_steps, root, MAX_SUBAGENTS)
    }

    /// `limit` di-clamp ke `1..=MAX_SUBAGENTS`: config tidak boleh menaikkan
    /// batas keras, hanya menurunkannya.
    pub fn with_capacity(
        session: Arc<ModelSession>,
        system: Option<String>,
        max_steps: u32,
        root: PathBuf,
        limit: usize,
    ) -> Self {
        let capacity = limit.clamp(1, MAX_SUBAGENTS);
        Self {
            session,
            system,
            max_steps,
            root,
            semaphore: Arc::new(Semaphore::new(capacity)),
            capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Jalankan banyak tugas (sisanya antre di semaphore).
    pub async fn run_many(&self, tasks: Vec<String>) -> Vec<Result<String, String>> {
        let futures = tasks.into_iter().map(|task| self.run_one(task));
        futures_util::future::join_all(futures).await
    }

    async fn run_one(&self, task: String) -> Result<String, String> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("sub-agent slot: {e}"))?;

        // Snapshot di awal tiap tugas: kalau pengguna mengganti model, tugas
        // berikutnya memakai model baru tanpa membangun ulang tool registry.
        let snapshot = self
            .session
            .snapshot()
            .ok_or_else(|| "no model connected (use /connect in TUI)".to_string())?;

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tools::read_file::ReadFile));
        registry.register(Box::new(tools::grep::Grep));
        registry.register(Box::new(tools::glob::Glob));
        // CATATAN: delegate_task TIDAK didaftarkan di sini (tanpa nesting).

        let policy = SimplePolicy::new(self.root.clone(), PermissionMode::ReadOnly);
        let ctx = ToolContext {
            cwd: self.root.clone(),
            permission_mode: PermissionMode::ReadOnly,
        };

        let system = format!(
            "You are a read-only sub-agent. Investigate the task and return a concise report. \
Do not attempt to modify files.\n\n{}",
            self.system.clone().unwrap_or_default()
        );

        let mut history = ContextStore::new();
        history.push(Message::user(task));

        let options = RunOptions {
            model: snapshot.model.clone(),
            system: Some(system),
            max_steps: self.max_steps,
            max_tokens: None,
            context_window_tokens: Some(snapshot.context_tokens),
            keep_recent: minic_core::compaction::DEFAULT_KEEP_RECENT,
        };
        let mut on_event = |_event| {};

        let outcome = run_agent(
            snapshot.provider.as_ref(),
            &registry,
            &policy,
            &minic_core::agent::NoopHooks,
            &mut history,
            &ctx,
            &options,
            None,
            &mut on_event,
        )
        .await
        .map_err(|e| e.to_string())?;

        if !outcome.text.trim().is_empty() {
            return Ok(outcome.text);
        }
        // Fallback: pesan assistant terakhir yang tidak kosong.
        let last = history.messages().iter().rev().find_map(|message| {
            if message.role == Role::Assistant {
                let text = message.text();
                (!text.trim().is_empty()).then_some(text)
            } else {
                None
            }
        });
        Ok(last.unwrap_or_else(|| "(sub-agent produced no text)".to_string()))
    }
}
