//! Helper bersama untuk tool host.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Direktori yang selalu dilewati saat menelusuri workspace.
pub const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", ".minic"];

/// Batas ukuran berkas yang dibaca tool pencarian (2 MB).
pub const MAX_SCAN_FILE_BYTES: u64 = 2_000_000;

pub fn skip_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
}

/// Tampilan path relatif ke root, dengan pemisah `/` (portable untuk model).
pub fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Tulis berkas secara atomik: tulis ke berkas temp lalu `rename`.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = parent.join(format!(".{stem}.tmp-{}", unique_suffix()));
    std::fs::write(&tmp, content)?;
    // `fs::rename` menimpa berkas tujuan yang ada (MOVEFILE_REPLACE_EXISTING di Windows).
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Potong teks pada batas karakter dengan penanda eksplisit (jangan diam-diam).
pub fn cap_text(text: &str, cap: usize) -> (String, bool) {
    if text.chars().count() <= cap {
        (text.to_string(), false)
    } else {
        let mut out: String = text.chars().take(cap).collect();
        out.push_str("\n… [truncated]");
        (out, true)
    }
}

/// Jumlah byte berkas tanpa membuka isinya (None bila gagal/ bukan berkas).
pub fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}
