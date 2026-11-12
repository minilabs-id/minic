//! Helper bersama untuk tool host.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Direktori yang **selalu** dilewati saat menelusuri workspace.
///
/// Ini hanya jaring pengaman: sumber kebenaran sebenarnya adalah `.gitignore`
/// proyek (lihat [`WalkFilter`]). Daftar ini menangani kasus tidak ada
/// `.gitignore` sama sekali.
pub const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", ".minic"];

/// Batas ukuran berkas yang dibaca tool pencarian (2 MB).
pub const MAX_SCAN_FILE_BYTES: u64 = 2_000_000;

pub fn skip_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
}

/// Satu aturan `.gitignore`.
#[derive(Debug, Clone)]
struct IgnoreRule {
    matcher: globset::GlobMatcher,
    /// `!pattern` — mengembalikan berkas yang tadinya diabaikan.
    negated: bool,
    /// Pola berakhiran `/` → hanya cocok untuk direktori.
    dir_only: bool,
    /// Pola yang di-anchor (`/foo`) relatif terhadap akar, bukan sembarang level.
    anchored: bool,
}

/// Filter penelusuran yang menghormati `.gitignore` bertingkat.
///
/// Tanpa ini, `grep`/`glob` memindai `dist/`, `build/`, `.venv/`, `__pycache__/`,
/// `vendor/` — memboroskan token dan menyesatkan model (mis. "definisi" yang
/// ditemukan di bundel hasil build). Terbukti: `grep` mengembalikan
/// `dist/bundle.js` walau `.gitignore` proyek menyebut `dist/`.
#[derive(Debug, Default)]
pub struct WalkFilter {
    root: PathBuf,
    /// Aturan per direktori (induk → aturan dari `.gitignore` di sana).
    rules: Vec<(PathBuf, Vec<IgnoreRule>)>,
}

impl WalkFilter {
    /// Muat `.gitignore` dari akar dan semua subdirektori (best-effort: berkas
    /// yang tidak terbaca dilewati, bukan menggagalkan pencarian).
    pub fn load(root: &Path) -> Self {
        let mut rules = Vec::new();
        collect_ignores(root, &mut rules, 0);
        // `.git/info/exclude` ikut dihormati — git memakainya, jadi pengguna
        // mengharapkan hal yang sama dari pencarian.
        let exclude = root.join(".git").join("info").join("exclude");
        if let Ok(text) = std::fs::read_to_string(&exclude) {
            let parsed = parse_rules(&text);
            if !parsed.is_empty() {
                rules.push((root.to_path_buf(), parsed));
            }
        }
        Self {
            root: root.to_path_buf(),
            rules,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Apakah path ini harus dilewati?
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        if skip_dir_name(path) {
            return true;
        }
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() {
            return false;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Aturan yang lebih spesifik (direktori lebih dalam) menang, dan
        // negasi (`!x`) menang atas abaikan — sama seperti git.
        let mut ignored = false;
        for (base, rules) in &self.rules {
            let base_rel = base
                .strip_prefix(&self.root)
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .replace('\\', "/");
            // Aturan hanya berlaku untuk path di dalam direktorinya.
            let scoped = if base_rel.is_empty() {
                rel.clone()
            } else if let Some(rest) = rel.strip_prefix(&format!("{base_rel}/")) {
                rest.to_string()
            } else {
                continue;
            };

            for rule in rules {
                if rule.dir_only && !is_dir {
                    continue;
                }
                let hit = if rule.anchored {
                    rule.matcher.is_match(&scoped)
                } else {
                    // Pola tak ber-anchor cocok pada nama berkas atau path penuh,
                    // sehingga `target` mengabaikan direktori di level mana pun.
                    rule.matcher.is_match(&scoped) || rule.matcher.is_match(&name)
                };
                if hit {
                    ignored = !rule.negated;
                }
            }
        }
        ignored
    }
}

/// Nama direktori yang selalu dilewati, apa pun isi `.gitignore`.
fn skip_dir_name(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .is_some_and(|name| IGNORED_DIRS.contains(&name.as_str()))
}

/// Batas kedalaman agar tidak menelusuri repo raksasa tanpa henti.
const MAX_IGNORE_DEPTH: usize = 8;

fn collect_ignores(dir: &Path, out: &mut Vec<(PathBuf, Vec<IgnoreRule>)>, depth: usize) {
    if depth > MAX_IGNORE_DEPTH {
        return;
    }
    let file = dir.join(".gitignore");
    if let Ok(text) = std::fs::read_to_string(&file) {
        let parsed = parse_rules(&text);
        if !parsed.is_empty() {
            out.push((dir.to_path_buf(), parsed));
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || skip_dir_name(&path) {
            continue;
        }
        collect_ignores(&path, out, depth + 1);
    }
}

fn parse_rules(text: &str) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, body) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let dir_only = body.ends_with('/');
        let anchored = body.starts_with('/') || body[..body.len().saturating_sub(1)].contains('/');
        let body = body.trim_start_matches('/').trim_end_matches('/');
        if body.is_empty() {
            continue;
        }
        // Pola gitignore setara globset: `dir/` → `dir` dan `dir/**`.
        let patterns = if dir_only {
            vec![format!("{body}/**"), body.to_string()]
        } else {
            vec![body.to_string()]
        };
        for pattern in patterns {
            if let Ok(glob) = globset::GlobBuilder::new(&pattern)
                .literal_separator(false)
                .build()
            {
                rules.push(IgnoreRule {
                    matcher: glob.compile_matcher(),
                    negated,
                    dir_only,
                    anchored,
                });
            }
        }
    }
    rules
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

/// Deteksi berkas biner tanpa membaca seluruh isi.
///
/// Heuristik: NUL byte di 8 KB pertama → biner (sama seperti `git`). Cukup
/// untuk menolak gambar/arsip/executable sebelum `read_to_string` gagal dengan
/// pesan UTF-8 yang membingungkan. Mengembalikan ukuran berkas bila biner.
pub fn binary_probe(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut head = [0u8; 8192];
    let read = file.read(&mut head).ok()?;
    if head[..read].contains(&0) {
        Some(size)
    } else {
        None
    }
}
