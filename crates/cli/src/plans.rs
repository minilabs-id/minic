//! Rencana tersimpan: daftar tugas investigasi yang bisa **ditinjau dulu**
//! sebelum sub-agent dijalankan.
//!
//! Berkas: `<root>/.minic/plans/<id>.json`.
//!
//! Alasan modul ini ada: `delegate_task` sebelumnya hanya menerima array tugas
//! inline, jadi tidak ada artefak yang bisa dibaca manusia/di-review sebelum
//! token dibakar, dan rencana tidak bisa diulang atau dipakai ulang. Dengan
//! plan tersimpan, alurnya menjadi:
//!
//! 1. `delegate_task { tasks: […], plan_id: "audit-x" }` → rencana **disimpan**
//!    dan **tidak** dijalankan (mode `save`).
//! 2. Manusia meninjau: `minic --plans` (daftar) / `--plan-show <id>` (isi).
//! 3. `delegate_task { plan_id: "audit-x", run: true }` → baru dieksekusi.
//!
//! Rencana disimpan sebagai JSON biasa (bukan DB) supaya bisa di-commit,
//! di-diff, dan dibaca tanpa membuka minic.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::minic_dir;

/// Status sebuah rencana. Hanya `Draft` yang boleh dijalankan tanpa `run: true`
/// eksplisit; `Executed` dicatat agar menjalankan ulang terlihat sengaja.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStatus {
    Draft,
    Executed,
}

impl PlanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanStatus::Draft => "draft",
            PlanStatus::Executed => "executed",
        }
    }
}

/// Satu rencana. `id` adalah nama berkas, jadi divalidasi ketat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub tasks: Vec<String>,
    pub status: PlanStatus,
    /// Unix epoch milidetik saat dibuat.
    pub created_ms: u128,
    /// Unix epoch milidetik eksekusi terakhir, bila pernah dijalankan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_ms: Option<u128>,
}

/// Direktori rencana: `<root>/.minic/plans`.
pub fn plans_dir(root: &Path) -> PathBuf {
    minic_dir(root).join("plans")
}

/// `id` harus aman sebagai nama berkas: huruf/angka/`-`/`_`, maksimum 64.
/// Tanpa validasi ini, `plan_id: "../config"` bisa menulis di luar direktori
/// rencana — jalur path traversal yang nyata.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("plan id must not be empty".to_string());
    }
    if id.len() > 64 {
        return Err("plan id must be at most 64 characters".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "plan id may only contain letters, digits, '-' and '_' (got {id:?})"
        ));
    }
    Ok(())
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Simpan rencana (menimpa yang sudah ada dengan id sama).
pub fn save(root: &Path, plan: &Plan) -> Result<PathBuf, String> {
    validate_id(&plan.id)?;
    let dir = plans_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.json", plan.id));
    let text = serde_json::to_string_pretty(plan).map_err(|e| format!("serialize plan: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Muat rencana berdasarkan id. `None` bila berkas tidak ada; `Err` bila rusak
/// (rusak harus terlihat, bukan diam-diam dianggap tidak ada).
pub fn load(root: &Path, id: &str) -> Result<Option<Plan>, String> {
    validate_id(id)?;
    let path = plans_dir(root).join(format!("{id}.json"));
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("plan {id} is corrupt: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Daftar semua rencana, diurutkan menurut id agar keluaran deterministik.
/// Berkas rusak dilewati dengan peringatan, bukan menggagalkan seluruh daftar.
pub fn list(root: &Path) -> Vec<(Plan, Option<String>)> {
    let dir = plans_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    names.sort();

    names
        .into_iter()
        .map(|path| {
            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            match std::fs::read_to_string(&path) {
                Ok(text) => match serde_json::from_str::<Plan>(&text) {
                    Ok(plan) => (plan, None),
                    Err(e) => (
                        Plan {
                            id,
                            tasks: Vec::new(),
                            status: PlanStatus::Draft,
                            created_ms: 0,
                            executed_ms: None,
                        },
                        Some(format!("corrupt: {e}")),
                    ),
                },
                Err(e) => (
                    Plan {
                        id,
                        tasks: Vec::new(),
                        status: PlanStatus::Draft,
                        created_ms: 0,
                        executed_ms: None,
                    },
                    Some(format!("unreadable: {e}")),
                ),
            }
        })
        .collect()
}

/// Rencana baru berstatus draft.
pub fn draft(id: impl Into<String>, tasks: Vec<String>) -> Plan {
    Plan {
        id: id.into(),
        tasks,
        status: PlanStatus::Draft,
        created_ms: now_ms(),
        executed_ms: None,
    }
}

/// Tandai rencana sudah dieksekusi lalu simpan.
pub fn mark_executed(root: &Path, plan: &mut Plan) -> Result<(), String> {
    plan.status = PlanStatus::Executed;
    plan.executed_ms = Some(now_ms());
    save(root, plan).map(|_| ())
}
