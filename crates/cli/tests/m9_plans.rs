//! M9: rencana tersimpan (`.minic/plans/`).
//!
//! Yang dibuktikan di sini adalah **keamanan dan kejelasan**, bukan kosmetik:
//! id rencana tidak boleh keluar dari direktori rencana, rencana rusak harus
//! terlihat (bukan dianggap tidak ada), dan `delegate_task` tidak boleh
//! menjalankan sub-agent saat mode simpan.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::model_session::ModelSession;
use minic::plans::{self, PlanStatus};
use minic::subagent::SubAgentPool;
use minic::tools::delegate::DelegateTask;
use minic_core::tool::{Tool, ToolContext};
use serde_json::json;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_m9_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn delegate(root: &Path) -> DelegateTask {
    let session = Arc::new(ModelSession::new(None, "test-model", 8_000));
    DelegateTask::new(Arc::new(SubAgentPool::with_capacity(
        session,
        None,
        3,
        root.to_path_buf(),
        minic::subagent::MAX_SUBAGENTS,
    )))
}

fn ctx(root: &Path) -> ToolContext {
    ToolContext {
        cwd: root.to_path_buf(),
    }
}

#[test]
fn plan_ids_cannot_escape_the_plans_directory() {
    for bad in [
        "../config",
        "..",
        "a/b",
        r"a\\b",
        "",
        "has space",
        "quote\"",
        &"x".repeat(65),
    ] {
        assert!(
            plans::validate_id(bad).is_err(),
            "expected {bad:?} to be rejected"
        );
    }
    for good in ["audit", "audit-auth", "audit_auth_2", "A1", "x"] {
        assert!(plans::validate_id(good).is_ok(), "{good:?} should be ok");
    }
}

#[test]
fn save_load_roundtrip_preserves_order() {
    let root = temp_root("roundtrip");
    let plan = plans::draft("p1", vec!["first".into(), "second".into()]);
    let path = plans::save(&root, &plan).expect("save");
    assert!(path.exists());

    let loaded = plans::load(&root, "p1").expect("load").expect("present");
    assert_eq!(loaded.id, "p1");
    assert_eq!(loaded.tasks, vec!["first", "second"]);
    assert_eq!(loaded.status, PlanStatus::Draft);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_plan_is_none_but_corrupt_plan_is_an_error() {
    let root = temp_root("corrupt");
    assert!(
        plans::load(&root, "absent")
            .expect("absent is ok")
            .is_none()
    );

    let dir = plans::plans_dir(&root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("broken.json"), "{ not json").unwrap();

    let err = plans::load(&root, "broken").expect_err("corrupt must error");
    assert!(err.contains("corrupt"), "message: {err}");

    // Daftar tetap bisa ditampilkan, dengan peringatan, alih-alih kosong.
    let listed = plans::list(&root);
    assert_eq!(listed.len(), 1);
    assert!(
        listed[0]
            .1
            .as_deref()
            .is_some_and(|w| w.contains("corrupt"))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_is_sorted_and_skips_non_json() {
    let root = temp_root("list");
    for id in ["zeta", "alpha", "mid"] {
        plans::save(&root, &plans::draft(id, vec![format!("task {id}")])).unwrap();
    }
    std::fs::write(plans::plans_dir(&root).join("notes.txt"), "ignore me").unwrap();

    let ids: Vec<String> = plans::list(&root).into_iter().map(|(p, _)| p.id).collect();
    assert_eq!(ids, vec!["alpha", "mid", "zeta"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mark_executed_persists_status() {
    let root = temp_root("executed");
    let mut plan = plans::draft("done", vec!["t".into()]);
    plans::save(&root, &plan).unwrap();
    plans::mark_executed(&root, &mut plan).unwrap();

    let loaded = plans::load(&root, "done").unwrap().unwrap();
    assert_eq!(loaded.status, PlanStatus::Executed);
    assert!(loaded.executed_ms.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn delegate_saves_a_plan_without_running_it() {
    let root = temp_root("save_mode");
    let tool = delegate(&root);

    let result = tool
        .execute(
            json!({ "plan_id": "review-me", "tasks": ["look at auth", "look at tests"] }),
            &ctx(&root),
        )
        .await
        .unwrap();

    // Tanpa provider, sub-agent tidak mungkin berhasil; kalau mode simpan bocor
    // ke eksekusi, hasil ini akan berupa error, bukan "plan saved".
    assert!(!result.is_error, "save must not run: {}", result.content);
    assert!(result.content.contains("plan saved (not executed)"));

    let saved = plans::load(&root, "review-me").unwrap().expect("saved");
    assert_eq!(saved.tasks.len(), 2);
    assert_eq!(saved.status, PlanStatus::Draft, "belum dieksekusi");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn delegate_rejects_bad_plan_id_without_writing() {
    let root = temp_root("bad_id");
    let tool = delegate(&root);
    let result = tool
        .execute(
            json!({ "plan_id": "../escape", "tasks": ["x"] }),
            &ctx(&root),
        )
        .await
        .unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("may only contain"));
    assert!(
        !root.join(".minic").join("plans").exists(),
        "tidak boleh menulis apa pun untuk id tidak valid"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn delegate_run_without_saved_plan_fails_clearly() {
    let root = temp_root("no_plan");
    let tool = delegate(&root);
    let result = tool
        .execute(json!({ "plan_id": "ghost", "run": true }), &ctx(&root))
        .await
        .unwrap();

    assert!(result.is_error);
    assert!(
        result.content.contains("no saved plan"),
        "{}",
        result.content
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn run_without_plan_id_is_rejected() {
    let root = temp_root("run_no_id");
    let tool = delegate(&root);
    let result = tool
        .execute(json!({ "tasks": ["x"], "run": true }), &ctx(&root))
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(
        result.content.contains("only meaningful with plan_id"),
        "{}",
        result.content
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn task_limit_still_applies_to_saved_plans() {
    let root = temp_root("limit");
    let session = Arc::new(ModelSession::new(None, "m", 8_000));
    let tool = DelegateTask::new(Arc::new(SubAgentPool::with_capacity(
        session,
        None,
        3,
        root.clone(),
        2, // batas diturunkan ke 2
    )));

    let result = tool
        .execute(
            json!({ "plan_id": "too-big", "tasks": ["a", "b", "c"] }),
            &ctx(&root),
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("too many sub-agents"));
    let _ = std::fs::remove_dir_all(&root);
}
