//! Penyimpanan API key lokal: `.minic/auth.json` (mode 0600 di Unix).
//!
//! Dipisah dari `config.toml` supaya berkas config bisa di-commit tanpa
//! membocorkan rahasia.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct Auth {
    pub api_key: Option<String>,
}

pub fn auth_path(root: &Path) -> PathBuf {
    crate::config::minic_dir(root).join("auth.json")
}

pub fn load(root: &Path) -> Auth {
    match std::fs::read_to_string(auth_path(root)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Auth::default(),
    }
}

/// Tulis API key ke `.minic/auth.json`. Mengembalikan path yang ditulis.
pub fn save(root: &Path, api_key: &str) -> std::io::Result<PathBuf> {
    let path = auth_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({ "api_key": api_key });
    let bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
    std::fs::write(&path, bytes)?;
    set_private(&path);
    Ok(path)
}

fn set_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        // Windows: berkas berada di profil pengguna; ACL lanjutan belum diatur.
        let _ = path;
    }
}
