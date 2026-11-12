//! M6-b: checkpoint & undo berkas.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::checkpoints;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_ckpt_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

#[test]
fn undo_restores_previous_content() {
    let root = temp_root("restore");
    let file = root.join("a.txt");
    std::fs::write(&file, "orig").unwrap();

    checkpoints::snapshot(&root, &file).unwrap();
    std::fs::write(&file, "changed").unwrap();

    let message = checkpoints::undo_last(&root).unwrap();
    assert!(message.contains("restored"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "orig");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn undo_removes_file_created_after_snapshot() {
    let root = temp_root("remove");
    let file = root.join("new.txt");

    // Belum ada saat snapshot.
    checkpoints::snapshot(&root, &file).unwrap();
    std::fs::write(&file, "created").unwrap();
    assert!(file.exists());

    checkpoints::undo_last(&root).unwrap();
    assert!(!file.exists(), "berkas baru harus dihapus oleh undo");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn undo_without_checkpoints_errors() {
    let root = temp_root("empty");
    assert!(checkpoints::undo_last(&root).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn undo_walks_backwards() {
    let root = temp_root("walk");
    let file = root.join("v.txt");

    std::fs::write(&file, "v1").unwrap();
    checkpoints::snapshot(&root, &file).unwrap();
    std::fs::write(&file, "v2").unwrap();

    checkpoints::snapshot(&root, &file).unwrap();
    std::fs::write(&file, "v3").unwrap();

    checkpoints::undo_last(&root).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2");
    checkpoints::undo_last(&root).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_reports_entries() {
    let root = temp_root("list");
    let file = root.join("x.txt");
    std::fs::write(&file, "x").unwrap();
    checkpoints::snapshot(&root, &file).unwrap();
    assert_eq!(checkpoints::list(&root).len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// Git checkpoint: snapshot utuh workspace via `refs/minic/*` (tanpa menyentuh
/// index/HEAD), lalu `--undo` memulihkan worktree dari commit itu.
///
/// Keterbatasan: `git stash create` hanya menangkap berkas yang di-track git;
/// berkas untracked tetap ditangani lapisan file-based. Bila `git` tidak
/// tersedia di host, test di-skip dengan alasan jelas.
fn git_root(tag: &str) -> Option<PathBuf> {
    let root = temp_root(tag);
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok()?;
    // Identitas git wajib untuk `stash create` → beri nilai lokal.
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&root)
        .output();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "minic test"])
        .current_dir(&root)
        .output();
    Some(root)
}

#[test]
fn git_snapshot_restores_whole_tree() {
    let Some(root) = git_root("git") else {
        eprintln!("skip: git not available on this host");
        return;
    };

    // Basis: satu commit dengan `tracked.txt` berisi "orig".
    std::fs::write(root.join("tracked.txt"), "orig").unwrap();
    let ok = std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&root)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        let _ = std::fs::remove_dir_all(&root);
        eprintln!("skip: git add failed on this host");
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "base"])
        .current_dir(&root)
        .output();

    // Worktree jadi kotor (seperti setelah agen mengedit berkas) → inilah
    // state yang harus dicapture snapshot.
    let target = root.join("tracked.txt");
    std::fs::write(&target, "state-a").unwrap();

    // Mutasi: ubah tracked + buat untracked.
    checkpoints::snapshot(&root, &target).unwrap();
    std::fs::write(&target, "state-b").unwrap();
    std::fs::write(root.join("untracked.txt"), "extra").unwrap();

    assert!(
        checkpoints::list(&root)
            .iter()
            .any(|line| line.starts_with("git:")),
        "refs/minic harus tampil di list: {:?}",
        checkpoints::list(&root)
    );

    let message = checkpoints::undo_last(&root).unwrap();
    assert!(message.contains("git checkpoint"), "{message}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "state-a",
        "tracked harus kembali ke state saat snapshot"
    );
    // Keterbatasan terdokumentasi: `git stash create` hanya menangkap berkas
    // yang di-track git; berkas untracked tetap ditangani lapisan file.
    assert!(
        root.join("untracked.txt").exists(),
        "untracked tidak disentuh lapisan git"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn git_undo_walks_back_consuming_refs() {
    let Some(root) = git_root("walk") else {
        eprintln!("skip: git not available on this host");
        return;
    };

    std::fs::write(root.join("tracked.txt"), "v0").unwrap();
    let ok = std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&root)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        let _ = std::fs::remove_dir_all(&root);
        eprintln!("skip: git add failed on this host");
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "base"])
        .current_dir(&root)
        .output();

    let target = root.join("tracked.txt");
    // Snapshot 1: state v1.
    std::fs::write(&target, "v1").unwrap();
    checkpoints::snapshot(&root, &target).unwrap();
    std::fs::write(&target, "v2").unwrap();

    // Snapshot 2: state v2.
    checkpoints::snapshot(&root, &target).unwrap();
    std::fs::write(&target, "v3").unwrap();

    assert_eq!(
        checkpoints::list(&root)
            .iter()
            .filter(|l| l.starts_with("git:"))
            .count(),
        2
    );

    // Undo 1 → kembali ke v2 (snapshot 2).
    let message = checkpoints::undo_last(&root).unwrap();
    assert!(message.contains("git checkpoint"), "{message}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");

    // Undo 2 → kembali ke v1 (snapshot 1). Ref harus dikonsumsi tiap undo.
    let message = checkpoints::undo_last(&root).unwrap();
    assert!(message.contains("git checkpoint"), "{message}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");

    assert_eq!(
        checkpoints::list(&root)
            .iter()
            .filter(|l| l.starts_with("git:"))
            .count(),
        0,
        "refs harus habis setelah dua undo"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ── Snapshot whole-tree untuk `bash` (M7) ────────────────────────────────
//
// `bash` tidak di-checkpoint per berkas: ia bisa menyentuh banyak path atau
// path di luar jail. Untuk perintah berisiko, snapshot whole-tree git disimpan
// lebih dulu supaya `--undo` bisa memulihkan kerusakan.

fn git(root: &PathBuf, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Siapkan repo git kecil dengan satu commit (butuh identitas git lokal).
fn init_repo(root: &PathBuf) -> bool {
    if !git(root, &["init"]) {
        return false;
    }
    git(root, &["config", "user.email", "minic@example.test"]);
    git(root, &["config", "user.name", "minic test"]);
    std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "init"])
}

#[test]
fn tree_snapshot_restores_bash_damage_in_git_repo() {
    let root = temp_root("tree_restore");
    if !init_repo(&root) {
        eprintln!("skip: git unavailable");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    // Snapshot diambil saat tree bersih → tidak ada perubahan untuk
    // di-capture, jadi `git stash create` mengembalikan kosong.
    // Agar snapshot berisi, buat perubahan lebih dulu (mis. kerja yang sudah
    // ada sebelum perintah berisiko dijalankan), lalu uji pemulihannya.
    std::fs::write(root.join("tracked.txt"), "in-progress work\n").unwrap();

    let outcome = checkpoints::snapshot_tree(&root);
    assert!(
        outcome.is_stored(),
        "expected stored snapshot, got {outcome:?}"
    );
    // Perintah berisiko merusak berkas setelah snapshot.
    std::fs::write(root.join("tracked.txt"), "destroyed by bash\n").unwrap();

    let message = checkpoints::undo_last(&root).unwrap();
    assert!(message.contains("restored"), "message: {message}");
    // Bandingkan setelah normalisasi CRLF: git/`restore` bisa menulis LF→CRLF
    // di checkout Windows, dan itu bukan yang sedang diuji di sini.
    let restored = std::fs::read_to_string(root.join("tracked.txt"))
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(restored, "in-progress work\n");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tree_snapshot_reports_non_git_workspace() {
    let root = temp_root("tree_nongit");
    std::fs::write(root.join("a.txt"), "x").unwrap();
    assert_eq!(
        checkpoints::snapshot_tree(&root),
        checkpoints::TreeSnapshot::NotGit
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tree_snapshot_reports_nothing_to_capture() {
    let root = temp_root("tree_clean");
    if !init_repo(&root) {
        eprintln!("skip: git unavailable");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    // Working tree bersih → tidak ada yang perlu di-commit.
    assert_eq!(
        checkpoints::snapshot_tree(&root),
        checkpoints::TreeSnapshot::NothingToCapture
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tree_and_file_checkpoints_are_listed_with_distinct_labels() {
    let root = temp_root("tree_list");
    if !init_repo(&root) {
        eprintln!("skip: git unavailable");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    std::fs::write(root.join("tracked.txt"), "modified\n").unwrap();
    assert!(checkpoints::snapshot_tree(&root).is_stored());

    let entries = checkpoints::list(&root);
    assert!(
        entries.iter().any(|entry| entry.contains("\ttree\t")),
        "tree snapshot should be labelled: {entries:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
