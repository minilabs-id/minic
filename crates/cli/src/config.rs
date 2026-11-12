//! Konfigurasi **project-local**: `.minic/config.toml` di dalam workspace.
//!
//! Tidak ada config global; setiap proyek membawa pengaturannya sendiri.
//! Env var tetap bisa menimpa saat runtime.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const MINIC_DIR: &str = ".minic";

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub system: Option<String>,
    pub shell: Option<String>,
    pub sandbox: Option<String>,
    /// Substring perintah `bash` yang **selalu** butuh approval, di samping
    /// klasifikasi bawaan. Contoh: `bash_deny_extra = ["terraform apply"]`.
    #[serde(default)]
    pub bash_deny_extra: Vec<String>,
    /// Batas sub-agent paralel untuk `delegate_task`. Diturunkan saja
    /// (maksimum keras tetap 10). Berguna untuk membatasi biaya token.
    pub max_subagents: Option<usize>,
}

impl FileConfig {
    /// Muat config dari `<root>/.minic/config.toml`.
    /// Berkas tidak ada atau rusak → config kosong (jangan crash).
    pub fn load(root: &Path) -> Self {
        let path = config_path(root);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}

pub fn minic_dir(root: &Path) -> PathBuf {
    root.join(MINIC_DIR)
}

/// `<root>/.minic/config.toml`.
pub fn config_path(root: &Path) -> PathBuf {
    minic_dir(root).join("config.toml")
}

/// Pastikan artefak runtime di `.minic/` tidak ikut ter-commit. Config boleh
/// di-commit; sesi, trace, dan auth tidak.
pub fn ensure_gitignore(root: &Path) {
    let dir = minic_dir(root);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(".gitignore");
    if !path.exists() {
        let _ = std::fs::write(
            &path,
            "trace.jsonl\nsessions.db\nsessions.db-wal\nsessions.db-shm\nauth.json\n",
        );
    }
}
