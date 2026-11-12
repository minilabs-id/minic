//! Checkpoint berkas: snapshot sebelum mutasi, untuk `--undo`.
//!
//! Dua lapisan, best-effort:
//! 1. **File-based** (selalu): salinan berkas + manifest JSON di
//!    `.minic/checkpoints/<id>.{json,bak}`. `--undo` mengonsumsinya.
//! 2. **Git-based** (bila workspace adalah repo git & file sudah ada):
//!    `git stash create` (tidak menyentuh index/HEAD pengguna) lalu
//!    `git update-ref refs/minic/cp-<id>` → snapshot utuh bekerja. `--undo`
//!    memprioritaskan ref git dan **mengonsumsinya** (ref dihapus), sehingga
//!    undo berulang berjalan mundur; commit lama tetap di object store git.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_nanos() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string()
}

fn checkpoints_dir(root: &Path) -> PathBuf {
    crate::config::minic_dir(root).join("checkpoints")
}

fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn is_git_repo(root: &Path) -> bool {
    run_git(root, &["rev-parse", "--is-inside-work-tree"]).is_some_and(|out| out == "true")
}

fn git_ref_for(id: &str) -> String {
    format!("refs/minic/cp-{id}")
}

/// Prefiks ref khusus snapshot whole-tree dari `bash`. Berbeda dari checkpoint
/// berkas tunggal agar `--list-checkpoints` bisa membedakan keduanya dan agar
/// pemulihan tetap deterministik.
fn git_tree_ref_for(id: &str) -> String {
    format!("refs/minic/cp-tree-{id}")
}

/// Simpan snapshot utuh workspace ke `refs/minic/*`. Best-effort: bila bukan
/// repo git, tanpa identitas git, atau tanpa perubahan → `None`.
fn snapshot_git(root: &Path) -> Option<String> {
    snapshot_git_with(root, &git_ref_for)
}

/// Snapshot whole-tree dengan prefiks ref tertentu (`cp-` atau `cp-tree-`).
fn snapshot_git_with(root: &Path, ref_for: &dyn Fn(&str) -> String) -> Option<String> {
    if !is_git_repo(root) {
        return None;
    }
    let commit = run_git(root, &["stash", "create", "minic checkpoint"])?;
    if commit.is_empty() {
        return None; // tidak ada perubahan yang ter-capture
    }
    let id = now_nanos();
    run_git(root, &["update-ref", &ref_for(&id), &commit])?;
    Some(id)
}

/// Snapshot `target` sebelum diubah. Kembalikan id checkpoint file-based.
pub fn snapshot(root: &Path, target: &Path) -> std::io::Result<String> {
    let dir = checkpoints_dir(root);
    std::fs::create_dir_all(&dir)?;

    let id = now_nanos();

    let existed = target.exists();
    // Lapisan git hanya berarti untuk berkas yang sudah ada: file baru belum
    // di-track git (`stash create` tidak menangkap untracked).
    if existed {
        std::fs::copy(target, dir.join(format!("{id}.bak")))?;
        let _ = snapshot_git(root);
    }
    let meta = serde_json::json!({
        "target": target.to_string_lossy(),
        "existed": existed,
    });
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_vec(&meta).unwrap_or_default(),
    )?;
    Ok(id)
}

/// Hasil upaya snapshot whole-tree (dipakai `bash`).
///
/// `bash` adalah satu-satunya tool yang **tidak** di-checkpoint per berkas
/// (ia bisa menyentuh banyak path sekaligus, termasuk path di luar jail).
/// Untuk perintah berisiko, snapshot whole-tree git disimpan lebih dulu agar
/// `--undo` bisa memulihkan kerusakan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeSnapshot {
    /// Ref `refs/minic/*` baru tersimpan.
    Stored(String),
    /// Workspace bukan repo git: tidak ada whole-tree snapshot.
    NotGit,
    /// Repo git tetapi tidak ada perubahan yang bisa di-capture.
    NothingToCapture,
    /// `git` gagal untuk alasan lain (perlu diketahui pengguna).
    Failed(String),
}

impl TreeSnapshot {
    /// Pesan satu baris untuk trace/UI. Ini best-effort dan tidak pernah
    /// menghalangi perintah.
    pub fn describe(&self) -> String {
        match self {
            TreeSnapshot::Stored(id) => {
                format!("bash checkpoint {id} stored (undo restores the tree)")
            }
            TreeSnapshot::NotGit => {
                "bash checkpoint skipped: workspace is not a git repo".to_string()
            }
            TreeSnapshot::NothingToCapture => {
                "bash checkpoint skipped: no tracked changes to capture".to_string()
            }
            TreeSnapshot::Failed(reason) => format!("bash checkpoint failed: {reason}"),
        }
    }

    /// `true` bila tree benar-benar tersimpan (ada yang bisa di-undo).
    pub fn is_stored(&self) -> bool {
        matches!(self, TreeSnapshot::Stored(_))
    }
}

/// Simpan snapshot whole-tree sebelum `bash` menjalankan perintah berisiko.
///
/// Berbeda dari [`snapshot`], fungsi ini **tidak** menyalin satu berkas: ia
/// meminta git menyimpan seluruh working tree ke `refs/minic/cp-<id>`.
/// `--undo` memulihkan tree dari ref itu dan mengonsumsinya.
pub fn snapshot_tree(root: &Path) -> TreeSnapshot {
    if !is_git_repo(root) {
        return TreeSnapshot::NotGit;
    }
    // Direktori checkpoint memastikan `list()`/`undo` konsisten; kegagalan
    // menulis direktori tidak fatal untuk snapshot (git ref tetap bisa dibuat).
    let _ = std::fs::create_dir_all(checkpoints_dir(root));
    match snapshot_git_with(root, &git_tree_ref_for) {
        Some(id) => TreeSnapshot::Stored(id),
        // `snapshot_git` mengembalikan `None` untuk dua alasan: bukan repo
        // (sudah dicek di atas) atau tidak ada perubahan.
        None => match run_git(root, &["status", "--porcelain", "--untracked-files=no"]) {
            Some(status) if status.trim().is_empty() => TreeSnapshot::NothingToCapture,
            Some(_) => TreeSnapshot::Failed("git stash create returned no commit".to_string()),
            None => TreeSnapshot::Failed("git status failed".to_string()),
        },
    }
}

fn file_ids(root: &Path) -> Vec<String> {
    let dir = checkpoints_dir(root);
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".json").map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// Satu ref checkpoint git: nama penuh ref, id, dan commit.
struct GitRef {
    name: String,
    id: String,
    commit: String,
}

/// Semua ref `refs/minic/*`, urut naik berdasarkan id (id = waktu, jadi
/// elemen terakhir adalah snapshot terbaru). Mendukung dua prefiks:
/// `cp-` (berkas tunggal) dan `cp-tree-` (whole-tree dari `bash`).
fn git_refs(root: &Path) -> Vec<GitRef> {
    let Some(output) = run_git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/minic/",
        ],
    ) else {
        return Vec::new();
    };
    let mut refs: Vec<GitRef> = output
        .lines()
        .filter_map(|line| {
            let (name, commit) = line.split_once(' ')?;
            // Cek `cp-tree-` lebih dulu: ia juga diawali `cp-`.
            let id = name
                .strip_prefix("refs/minic/cp-tree-")
                .or_else(|| name.strip_prefix("refs/minic/cp-"))?
                .to_string();
            Some(GitRef {
                name: name.to_string(),
                id,
                commit: commit.to_string(),
            })
        })
        .collect();
    // Urutkan berdasarkan id, bukan nama ref: id adalah timestamp sehingga
    // urutan ini benar walau prefiks ref berbeda-beda.
    refs.sort_by(|a, b| a.id.cmp(&b.id));
    refs
}

/// Ref git terbaru (apa pun jenisnya).
fn latest_git_ref(root: &Path) -> Option<GitRef> {
    git_refs(root).pop()
}

fn read_target(dir: &Path, id: &str) -> Option<(PathBuf, bool)> {
    let raw = std::fs::read_to_string(dir.join(format!("{id}.json"))).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let target = PathBuf::from(value.get("target")?.as_str()?);
    let existed = value.get("existed")?.as_bool()?;
    Some((target, existed))
}

/// Pulihkan checkpoint terakhir (git lebih dulu, lalu file-based).
///
/// Lapisan git dikonsumsi: ref `refs/minic/cp-<id>` dihapus setelah restore,
/// sehingga `--undo` berulang berjalan mundur (commit lama tetap di object
/// store git sampai GC; kalau mau riwayat penuh, pakai `--list-checkpoints`).
pub fn undo_last(root: &Path) -> Result<String, String> {
    if let Some(git_ref) = latest_git_ref(root) {
        let restored = run_git(
            root,
            &["restore", "--source", &git_ref.commit, "--worktree", "."],
        );
        if restored.is_some() {
            let _ = run_git(root, &["update-ref", "-d", &git_ref.name]);
            let kind = if git_ref.name.contains("cp-tree-") {
                "tree checkpoint"
            } else {
                "git checkpoint"
            };
            return Ok(format!("{kind} {} restored to worktree", git_ref.id));
        }
        // restore gagal (mis. git terlalu tua) → lanjut ke lapisan file.
    }

    let dir = checkpoints_dir(root);
    let all = file_ids(root);
    let id = all
        .last()
        .cloned()
        .ok_or_else(|| "no checkpoints to undo".to_string())?;

    let (target, existed) =
        read_target(&dir, &id).ok_or_else(|| format!("checkpoint {id} is corrupt"))?;

    if existed {
        std::fs::copy(dir.join(format!("{id}.bak")), &target)
            .map_err(|e| format!("restore {}: {e}", target.display()))?;
    } else if target.exists() {
        std::fs::remove_file(&target).map_err(|e| format!("remove {}: {e}", target.display()))?;
    }

    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.bak")));

    let action = if existed { "restored" } else { "removed" };
    Ok(format!("{action} {}", target.display()))
}

/// Daftar checkpoint: baris file-based `id\ttarget`, lalu baris git
/// `git:<id>\t<commit>`.
pub fn list(root: &Path) -> Vec<String> {
    let dir = checkpoints_dir(root);
    let mut entries: Vec<String> = file_ids(root)
        .into_iter()
        .map(|id| {
            let target = read_target(&dir, &id)
                .map(|(path, _)| path.display().to_string())
                .unwrap_or_default();
            format!("{id}\t{target}")
        })
        .collect();

    for git_ref in git_refs(root) {
        // Ref whole-tree (dibuat oleh `bash`) diberi label agar berbeda dari
        // checkpoint berkas tunggal saat `--list-checkpoints`.
        let label = if git_ref.name.contains("cp-tree-") {
            "tree"
        } else {
            "file"
        };
        entries.push(format!("git:{}\t{label}\t{}", git_ref.name, git_ref.commit));
    }
    entries
}
