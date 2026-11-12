//! Fase C: pencarian menghormati `.gitignore`.
//!
//! Sebelum perbaikan, `grep` mengembalikan hasil dari `dist/` meskipun
//! `.gitignore` proyek menyebut `dist/` (daftar abaikan hanya 4 entri
//! hardcoded). Test ini mengunci perilaku yang benar, termasuk negasi.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use minic::tools::common::WalkFilter;
use minic::tools::glob::Glob;
use minic::tools::grep::Grep;
use minic_core::permission::PermissionMode;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_fasec_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn ctx(root: &Path) -> ToolContext {
    ToolContext {
        cwd: root.to_path_buf(),
        permission_mode: PermissionMode::Auto,
    }
}

#[tokio::test]
async fn grep_skips_gitignored_directories() {
    let root = temp_root("grep");
    write(&root, ".gitignore", "dist/\n");
    write(&root, "dist/bundle.js", "NEEDLE in dist\n");
    write(&root, "src/app.rs", "NEEDLE in src\n");

    let result = Grep
        .execute(json!({ "pattern": "NEEDLE" }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.content.contains("src/app.rs"), "{}", result.content);
    assert!(
        !result.content.contains("dist/"),
        "direktori yang di-.gitignore tidak boleh dipindai: {}",
        result.content
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn negation_pattern_is_honoured() {
    let root = temp_root("negation");
    write(&root, ".gitignore", "*.log\n");
    write(&root, ".gitignore", "*.log\n!keep.log\n");
    write(&root, "debug.log", "NEEDLE debug\n");
    write(&root, "keep.log", "NEEDLE keep\n");

    let result = Grep
        .execute(json!({ "pattern": "NEEDLE" }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.content.contains("keep.log"), "{}", result.content);
    assert!(!result.content.contains("debug.log"), "{}", result.content);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn glob_skips_gitignored_directories() {
    let root = temp_root("glob");
    write(&root, ".gitignore", "build/\n");
    write(&root, "build/out.js", "x");
    write(&root, "src/main.js", "y");

    let result = Glob
        .execute(json!({ "pattern": "**/*.js" }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.content.contains("src/main.js"), "{}", result.content);
    assert!(!result.content.contains("build/"), "{}", result.content);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn nested_gitignore_applies_to_its_subtree() {
    let root = temp_root("nested");
    write(&root, ".gitignore", ""); // kosong di akar
    write(&root, "vendor/.gitignore", "*.min.js\n");
    write(&root, "vendor/lib.min.js", "NEEDLE min\n");
    write(&root, "vendor/lib.js", "NEEDLE full\n");

    let result = Grep
        .execute(json!({ "pattern": "NEEDLE" }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.content.contains("lib.js"), "{}", result.content);
    assert!(!result.content.contains("lib.min.js"), "{}", result.content);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn without_gitignore_the_old_defaults_still_apply() {
    let root = temp_root("defaults");
    write(&root, "target/gen.rs", "NEEDLE target\n");
    write(&root, "src/app.rs", "NEEDLE src\n");
    // Tanpa `.gitignore`: `target/` tetap dilewati (jaring pengaman).

    let result = Grep
        .execute(json!({ "pattern": "NEEDLE" }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.content.contains("src/app.rs"), "{}", result.content);
    assert!(!result.content.contains("target/"), "{}", result.content);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn safe_dirs_are_never_ignored_even_if_gitignore_says_so() {
    let root = temp_root("safety");
    write(&root, ".gitignore", "!target/\n");
    let filter = WalkFilter::load(&root);
    assert!(
        filter.is_ignored(&root.join("target"), true),
        ".git harus selalu dilewati apa pun isi .gitignore"
    );
    assert!(filter.is_ignored(&root.join(".git"), true));
    let _ = std::fs::remove_dir_all(&root);
}
