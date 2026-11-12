//! Aturan path jail (leksikal + canonical bila memungkinkan) + daftar sensitif.
//!
//! Catatan Windows: `fs::canonicalize` mengembalikan path ber-prefix verbatim
//! `\\?\`. Karena itu **kedua sisi** harus lewat jalur yang sama (canonical
//! bila ada, normalisasi leksikal bila tidak), jika tidak `starts_with` gagal
//! membandingkan bentuk.

use std::path::{Component, Path, PathBuf};

/// Resolusi kandidat ke path absolut di dalam `root`.
///
/// Gagal bila keluar workspace (`..`, symlink keluar, path absolut di luar)
/// atau menunjuk berkas sensitif. Dipakai tool sebagai pertahanan berlapis,
/// terlepas dari keputusan policy.
pub fn resolve(root: &Path, candidate: &str) -> Result<PathBuf, String> {
    let root = canonical_root(root);
    let resolved = resolve_joined(&root, candidate);

    if !resolved.starts_with(&root) {
        return Err(format!("path outside workspace: {candidate}"));
    }
    if is_sensitive(&resolved) {
        return Err(format!("refusing sensitive path: {candidate}"));
    }
    Ok(resolved)
}

/// Apakah `candidate` berada di dalam `root`? (tanpa cek sensitif)
pub fn is_within(root: &Path, candidate: &str) -> bool {
    let root = canonical_root(root);
    resolve_joined(&root, candidate).starts_with(&root)
}

/// Resolusi kandidat relatif ke root yang **sudah** kanonik.
fn resolve_joined(root_canon: &Path, candidate: &str) -> PathBuf {
    let raw = Path::new(candidate);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root_canon.join(raw)
    };
    std::fs::canonicalize(&joined).unwrap_or_else(|_| normalize(&joined))
}

fn canonical_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| normalize(root))
}

/// Nama/direktori berkas yang tak boleh disentuh tool (kebocoran secret).
pub fn is_sensitive(path: &Path) -> bool {
    const SENSITIVE_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg"];
    const SENSITIVE_NAMES: &[&str] = &[
        ".git-credentials",
        ".netrc",
        "id_rsa",
        "id_ed25519",
        "id_ecdsa",
    ];

    for component in path.components() {
        if let Component::Normal(os) = component
            && SENSITIVE_DIRS.contains(&os.to_string_lossy().as_ref())
        {
            return true;
        }
    }

    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_ascii_lowercase();
        if SENSITIVE_NAMES.contains(&lower.as_str()) {
            return true;
        }
        if lower.ends_with(".pem") || lower.ends_with(".key") {
            return true;
        }
    }
    false
}

/// Normalisasi leksikal tanpa menyentuh filesystem (menyelesaikan `.` dan `..`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn relative_inside_root_is_allowed() {
        let root = temp_root("minic_jail_a");
        assert!(is_within(&root, "src/main.rs"));
    }

    #[test]
    fn parent_escape_is_rejected() {
        let root = temp_root("minic_jail_b");
        assert!(!is_within(&root, "../elsewhere.txt"));
        assert!(!is_within(&root, "a/../../b.txt"));
    }

    #[test]
    fn absolute_outside_is_rejected() {
        let root = temp_root("minic_jail_c");
        let outside = std::env::temp_dir().join("minic_outside.txt");
        assert!(!is_within(&root, &outside.to_string_lossy()));
    }

    #[test]
    fn resolve_rejects_escape_and_accepts_inside() {
        let root = temp_root("minic_jail_d");
        assert!(resolve(&root, "ok.txt").is_ok());
        assert!(resolve(&root, "../escape.txt").is_err());
    }

    #[test]
    fn sensitive_paths_are_flagged() {
        assert!(is_sensitive(Path::new("/home/u/.ssh/id_rsa")));
        assert!(is_sensitive(Path::new("/x/server.pem")));
        assert!(is_sensitive(Path::new("/x/private.key")));
        assert!(!is_sensitive(Path::new("/x/src/main.rs")));
    }
}
