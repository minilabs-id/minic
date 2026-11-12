//! M8: nilai `.minic/config.toml` yang dulu dideklarasikan tetapi tidak pernah
//! dibaca — `shell`, `max_subagents` — serta peringatan post-hook yang dulu
//! hilang di stderr.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use minic::config::FileConfig;
use minic::subagent::{MAX_SUBAGENTS, SubAgentPool};
use minic::tools::bash::Bash;
use minic_core::permission::PermissionMode;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_m8_{tag}_{nanos}"));
    std::fs::create_dir_all(root.join(".minic")).unwrap();
    std::fs::canonicalize(&root).unwrap()
}

fn ctx_for(root: &Path) -> ToolContext {
    ToolContext {
        cwd: root.to_path_buf(),
        permission_mode: PermissionMode::Auto,
    }
}

#[test]
fn config_shell_is_read_from_toml() {
    let root = temp_root("shellcfg");
    std::fs::write(
        root.join(".minic").join("config.toml"),
        "shell = \"bash\"\nmax_subagents = 3\n",
    )
    .unwrap();
    let cfg = FileConfig::load(&root);
    assert_eq!(cfg.shell.as_deref(), Some("bash"));
    assert_eq!(cfg.max_subagents, Some(3));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn configured_shell_is_actually_used_to_run_commands() {
    let root = temp_root("shellrun");
    let ctx = ctx_for(&root);

    // `/bin/sh` dan `bash` sama-sama menerima `-c`; nilai config harus menang
    // atas default OS. Di Windows default-nya cmd.exe, jadi buktinya adalah
    // sintaks POSIX berhasil: `echo` biasa jalan di keduanya, tetapi `$((...))`
    // aritmetika hanya jalan di shell POSIX.
    let result = Bash::new("off")
        .with_shell(Some("sh".to_string()))
        .execute(json!({ "command": "echo $((6 * 7))" }), &ctx)
        .await
        .unwrap();

    let out = result.content.trim();
    assert!(
        out.contains("42"),
        "config shell did not take effect; output: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn subagent_capacity_can_only_be_lowered() {
    use minic::model_session::ModelSession;
    use std::sync::Arc;

    let session = Arc::new(ModelSession::new(None, "test-model", 8_000));
    let make = |limit: usize| {
        SubAgentPool::with_capacity(Arc::clone(&session), None, 5, PathBuf::from("/tmp"), limit)
            .capacity()
    };

    assert_eq!(make(3), 3, "config may lower the limit");
    assert_eq!(make(0), 1, "zero is clamped up, never freezing delegation");
    assert_eq!(
        make(99),
        MAX_SUBAGENTS,
        "config must not raise the hard cap"
    );
}
