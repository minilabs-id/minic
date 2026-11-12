//! Fase E: pengaturan biaya, deteksi biner, dan konteks pencarian.
//!
//! Ketiganya menutup kesenjangan yang membuat agen sulit dipakai di proyek
//! nyata: batas output tidak bisa diatur, berkas biner menghasilkan error io
//! mentah, dan `grep` hanya memberi satu baris tanpa konteks.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use minic::tools::grep::Grep;
use minic::tools::read_file::ReadFile;
use minic_core::permission::PermissionMode;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_fasee_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn ctx(root: &Path) -> ToolContext {
    ToolContext {
        cwd: root.to_path_buf(),
        permission_mode: PermissionMode::Auto,
    }
}

#[tokio::test]
async fn binary_file_is_reported_clearly() {
    let root = temp_root("binary");
    // NUL byte di awal → biner, bukan UTF-8 teks.
    std::fs::write(
        root.join("image.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
    )
    .unwrap();

    let result = ReadFile
        .execute(json!({ "path": "image.png" }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.is_error, "berkas biner harus ditolak jelas");
    assert!(result.content.contains("binary file"), "{}", result.content);
    assert!(
        result.content.contains("xxd") || result.content.contains("bash"),
        "harus menyarankan cara memeriksa: {}",
        result.content
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn text_file_still_reads_normally() {
    let root = temp_root("text");
    std::fs::write(root.join("a.rs"), "fn main() {}\n").unwrap();
    let result = ReadFile
        .execute(json!({ "path": "a.rs" }), &ctx(&root))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("fn main"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn grep_context_includes_surrounding_lines() {
    let root = temp_root("context");
    let body = "alpha\nbeta\nTARGET\ngamma\ndelta\n";
    std::fs::write(root.join("f.txt"), body).unwrap();

    let result = Grep
        .execute(json!({ "pattern": "TARGET", "context": 1 }), &ctx(&root))
        .await
        .unwrap();

    // Baris sekitar ikut tampil.
    assert!(result.content.contains("beta"), "{}", result.content);
    assert!(result.content.contains("gamma"), "{}", result.content);
    // Baris match memakai ':' dan konteks '-', agar model bisa membedakan.
    assert!(
        result.content.contains("f.txt:3:TARGET"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("f.txt-2-beta"),
        "{}",
        result.content
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn grep_without_context_keeps_the_old_single_line_format() {
    let root = temp_root("nocontext");
    std::fs::write(root.join("f.txt"), "alpha\nTARGET\n").unwrap();

    let result = Grep
        .execute(json!({ "pattern": "TARGET" }), &ctx(&root))
        .await
        .unwrap();

    assert!(
        result.content.contains("f.txt:2:TARGET"),
        "{}",
        result.content
    );
    assert!(!result.content.contains("alpha"), "{}", result.content);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_exposes_max_output_tokens() {
    // Bukti bahwa flag benar-benar ada dan terhubung (bukan hanya niat di
    // dokumentasi): `--help` harus menyebutnya.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_minic"))
        .arg("--help")
        .output()
        .expect("jalankan minic --help");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("--max-output-tokens"),
        "--help harus menyebut --max-output-tokens:
{text}"
    );
}
