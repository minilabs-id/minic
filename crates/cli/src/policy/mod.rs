//! Kebijakan izin.

pub mod commands;
pub mod jail;

use crate::approval::{self, ApprovalChoice, ApprovalRequest, Category, Scope};
use futures_core::future::BoxFuture;
use minic_core::permission::{Decision, PermissionHandler, PermissionMode};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::mpsc::UnboundedSender;

/// Jalur pertanyaan approval.
pub enum AskRoute {
    /// Prompt di terminal (mode linear). Non-TTY → fail-closed.
    /// `always` menyimpan scope yang diizinkan permanen selama proses ini.
    Terminal { always: Mutex<HashSet<Scope>> },
    /// Modal TUI lewat channel. `always` menyimpan scope yang sudah diizinkan
    /// permanen selama sesi (per nama tool **atau** per kategori).
    Tui {
        tx: UnboundedSender<ApprovalRequest>,
        always: Mutex<HashSet<Scope>>,
    },
}

/// Kebijakan M1/M2:
/// - Tolak akses path di luar workspace (hard-deny, berlaku semua mode).
/// - Tolak berkas sensitif (`.ssh`, `*.pem`, `*.key`, …).
/// - `readonly` menolak tool yang mengubah.
/// - `ask` meminta approval interaktif untuk tool yang mengubah; **non-TTY
///   fail-closed** (deny).
pub struct SimplePolicy {
    root: PathBuf,
    /// Mode bisa diganti saat sesi berjalan (`/discus` ↔ `/execute`), jadi
    /// disimpan atomik: pembacaan terjadi di jalur async, penulisan dari
    /// handler tombol TUI.
    mode: Arc<AtomicU8>,
    ask_route: AskRoute,
    /// `false` mematikan klasifikasi perintah `bash` (override eksplisit).
    guard_shell: bool,
    /// Pola tambahan yang selalu butuh approval (dari config pengguna).
    extra_deny: Vec<String>,
}

impl SimplePolicy {
    pub fn new(root: PathBuf, mode: PermissionMode) -> Self {
        Self {
            root,
            mode: Arc::new(AtomicU8::new(mode.to_u8())),
            ask_route: AskRoute::Terminal {
                always: Mutex::new(HashSet::new()),
            },
            guard_shell: true,
            extra_deny: Vec::new(),
        }
    }

    /// Ganti jalur approval (TUI memakai modal-channel, bukan stdin).
    pub fn with_ask_route(mut self, route: AskRoute) -> Self {
        self.ask_route = route;
        self
    }

    /// Matikan klasifikasi perintah `bash` (setara `MINIC_BASH_GUARD=off`).
    /// Perintah berisiko kembali diperlakukan seperti biasa. Tidak disarankan;
    /// tersedia agar pengguna yang paham risikonya bisa memilih.
    pub fn with_shell_guard(mut self, enabled: bool) -> Self {
        self.guard_shell = enabled;
        self
    }

    /// Daftar substring tambahan yang memaksa approval untuk `bash`
    /// (`[approval] bash_deny_extra` di `.minic/config.toml`).
    pub fn with_extra_deny(mut self, patterns: Vec<String>) -> Self {
        self.extra_deny = patterns;
        self
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn mode(&self) -> PermissionMode {
        PermissionMode::from_u8(self.mode.load(Ordering::SeqCst))
    }

    /// Ganti mode saat sesi berjalan. Dipakai TUI untuk beralih antara mode
    /// diskusi (read-only) dan eksekusi tanpa restart.
    pub fn set_mode(&self, mode: PermissionMode) {
        // Pakai `to_u8`, bukan `mode as u8`: diskriminan enum bisa berubah
        // urutan tanpa terlihat, dan `from_u8` di sisi baca memakai pemetaan
        // eksplisit. Dua pemetaan yang berbeda = bug yang menunggu.
        self.mode.store(mode.to_u8(), Ordering::SeqCst);
        // Izin "selalu" adalah keputusan untuk mode **saat itu**. Membawanya
        // melewati pergantian mode akan membuat tool yang tadinya tidak
        // berbahaya (mis. `bash` di eksekusi) tetap diizinkan saat pengguna
        // pindah ke diskusi. Buang supaya pergantian mode benar-benar
        // mengembalikan pengguna ke pertanyaan izin.
        let always = match &self.ask_route {
            AskRoute::Terminal { always } => always,
            AskRoute::Tui { always, .. } => always,
        };
        if let Ok(mut always) = always.lock() {
            always.clear();
        }
    }

    fn always_allowed(&self, tool_name: &str) -> bool {
        let category = Category::of_tool(tool_name);
        let lookup = |set: &Mutex<HashSet<Scope>>| {
            set.lock()
                .map(|set| {
                    set.contains(&Scope::Tool(tool_name.to_string()))
                        || set.contains(&Scope::Category(category))
                })
                .unwrap_or(false)
        };
        match &self.ask_route {
            AskRoute::Terminal { always } => lookup(always),
            AskRoute::Tui { always, .. } => lookup(always),
        }
    }

    /// Periksa apakah `bash` menjalankan perintah berisiko. Mengembalikan
    /// alasan approval bila ya.
    fn shell_risk(&self, args: &serde_json::Value) -> Option<String> {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if command.trim().is_empty() {
            return None;
        }
        // Override eksplisit pengguna menang atas klasifikasi bawaan.
        if !self.guard_shell {
            return None;
        }
        for pattern in &self.extra_deny {
            let needle = pattern.trim();
            if !needle.is_empty() && command.contains(needle) {
                return Some(format!("shell command matches local deny list ({needle})"));
            }
        }
        commands::classify(command)
            .label()
            .map(|label| format!("shell command is {label}: {}", first_line(command)))
    }
}

/// Baris pertama perintah, dipotong agar alasan approval tetap ringkas.
fn first_line(command: &str) -> String {
    let line = command.lines().next().unwrap_or("").trim();
    let mut chars = line.chars();
    let mut out: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

impl PermissionHandler for SimplePolicy {
    fn decide<'a>(
        &'a self,
        tool_name: &'a str,
        args: &'a serde_json::Value,
        mutating: bool,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            // Hard-deny: jail + berkas sensitif (berlaku di semua mode).
            if let Some(path) = args.get("path").and_then(|v| v.as_str())
                && let Err(reason) = jail::resolve(&self.root, path)
            {
                return Decision::Deny(reason);
            }

            if mutating {
                match self.mode() {
                    PermissionMode::ReadOnly => {
                        return Decision::Deny("readonly mode: mutating tool denied".to_string());
                    }
                    PermissionMode::Ask => {
                        if self.always_allowed(tool_name) {
                            return Decision::Allow;
                        }
                        return Decision::Ask(format!("{tool_name} modifies the workspace"));
                    }
                    PermissionMode::Auto => {}
                }
            }

            // `bash` tidak dijail, jadi perintah berisiko butuh approval bahkan
            // di `--mode auto`. Jalur ini juga berlaku bila tool tidak mutating
            // (tidak terjadi saat ini, tapi menjaga invariant).
            if tool_name == "bash"
                && !self.always_allowed(tool_name)
                && let Some(reason) = self.shell_risk(args)
            {
                return Decision::Ask(reason);
            }

            Decision::Allow
        })
    }

    fn resolve_ask<'a>(
        &'a self,
        tool_name: &'a str,
        args: &'a serde_json::Value,
        reason: &'a str,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            match &self.ask_route {
                AskRoute::Terminal { always } => {
                    match approval::prompt_ask(tool_name, args, reason) {
                        ApprovalChoice::AllowOnce => Decision::Allow,
                        ApprovalChoice::AllowAlways => {
                            if let Ok(mut set) = always.lock() {
                                set.insert(Scope::Tool(tool_name.to_string()));
                            }
                            Decision::Allow
                        }
                        ApprovalChoice::AllowCategory(category) => {
                            if let Ok(mut set) = always.lock() {
                                set.insert(Scope::Category(category));
                            }
                            Decision::Allow
                        }
                        ApprovalChoice::Deny => {
                            if approval::is_interactive() {
                                Decision::Deny("declined by user".to_string())
                            } else {
                                Decision::Deny(
                                    "non-interactive session: approval denied (fail-closed)"
                                        .to_string(),
                                )
                            }
                        }
                    }
                }
                AskRoute::Tui { tx, always } => {
                    let (reply, response) = tokio::sync::oneshot::channel();
                    let request = ApprovalRequest {
                        tool: tool_name.to_string(),
                        summary: approval::summarize_args(tool_name, args),
                        reason: reason.to_string(),
                        category: Category::of_tool(tool_name),
                        details: approval::detail_args(args),
                        reply,
                    };
                    if tx.send(request).is_err() {
                        return Decision::Deny("approval channel closed (fail-closed)".to_string());
                    }
                    match response.await {
                        Ok(ApprovalChoice::AllowOnce) => Decision::Allow,
                        Ok(ApprovalChoice::AllowAlways) => {
                            if let Ok(mut set) = always.lock() {
                                set.insert(Scope::Tool(tool_name.to_string()));
                            }
                            Decision::Allow
                        }
                        Ok(ApprovalChoice::AllowCategory(category)) => {
                            if let Ok(mut set) = always.lock() {
                                set.insert(Scope::Category(category));
                            }
                            Decision::Allow
                        }
                        Ok(ApprovalChoice::Deny) => Decision::Deny("declined by user".to_string()),
                        Err(_) => {
                            Decision::Deny("approval channel closed (fail-closed)".to_string())
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod mode_switch_tests {
    use super::*;

    fn policy(mode: PermissionMode) -> SimplePolicy {
        SimplePolicy::new(std::path::PathBuf::from("."), mode)
    }

    /// `set_mode` harus benar-benar mengubah mode yang dibaca `decide`.
    #[test]
    fn set_mode_is_visible_to_decide() {
        let policy = policy(PermissionMode::Ask);
        assert_eq!(policy.mode(), PermissionMode::Ask);
        policy.set_mode(PermissionMode::ReadOnly);
        assert_eq!(policy.mode(), PermissionMode::ReadOnly);
        policy.set_mode(PermissionMode::Auto);
        assert_eq!(policy.mode(), PermissionMode::Auto);
    }

    /// `set_mode` menyimpan lewat `to_u8` dan `mode()` membacanya kembali;
    /// keduanya harus sepakat untuk **setiap** varian. Kalau salah satu memakai
    /// `as u8`, menambah varian baru akan menggeser nilainya diam-diam.
    #[test]
    fn mode_round_trips_through_the_atomic() {
        for mode in [
            PermissionMode::Auto,
            PermissionMode::Ask,
            PermissionMode::ReadOnly,
        ] {
            let policy = policy(PermissionMode::Auto);
            policy.set_mode(mode);
            assert_eq!(policy.mode(), mode, "mode {mode} tidak round-trip");
            // Nilai yang tersimpan juga harus konsisten dengan `from_u8`.
            assert_eq!(
                PermissionMode::from_u8(mode.to_u8()),
                mode,
                "to_u8/from_u8 tidak sepakat untuk {mode}"
            );
        }
    }

    /// `set_mode` membuang izin "selalu": keputusan itu milik mode lama.
    #[test]
    fn set_mode_discards_always_grants() {
        let policy = policy(PermissionMode::Auto).with_ask_route(AskRoute::Terminal {
            always: Mutex::new(HashSet::new()),
        });
        // Simulasi "izinkan selalu" untuk bash.
        let always = match &policy.ask_route {
            AskRoute::Terminal { always } => always,
            AskRoute::Tui { .. } => unreachable!(),
        };
        always
            .lock()
            .expect("lock")
            .insert(Scope::Tool("bash".to_string()));
        assert!(policy.always_allowed("bash"));

        policy.set_mode(PermissionMode::ReadOnly);
        assert!(
            !policy.always_allowed("bash"),
            "izin 'selalu' harus dibuang saat mode berganti"
        );
    }

    /// Mode diskusi menolak tool yang mengubah, apa pun izin sebelumnya.
    #[test]
    fn discuss_denies_mutating_tools() {
        let policy = policy(PermissionMode::ReadOnly);
        let args = serde_json::json!({ "path": "a.txt" });
        assert!(matches!(
            futures_executor_block_on(policy.decide("write_file", &args, true)),
            Decision::Deny(_)
        ));
    }

    /// Helper sinkron kecil supaya test tidak perlu runtime async penuh.
    fn futures_executor_block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }
}
