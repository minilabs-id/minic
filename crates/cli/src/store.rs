//! Persistensi sesi di SQLite (`rusqlite` bundled).
//!
//! Berkas: `<root>/.minic/sessions.db`.
//!
//! Penulisan bersifat **prefix-compare** (pelajaran baseline F-05): bandingkan
//! history tersimpan dengan yang baru per-pesan (bukan per-panjang), lalu:
//! - prefix sama → **append** hanya pesan baru (jalur normal),
//! - pesan baru memangkas ekor → **hapus** ekor (shrink/kompaksi),
//! - divergen di tengah → **tulis ulang** dari titik divergen.
//!
//! Perbandingan mencakup `reasoning` dan `is_error`, jadi perubahan pada field
//! itu tidak dianggap "tak berubah".

use minic_core::agent::HistoryPersistence;
use minic_core::types::{Content, Message, Role, ToolCall};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct SessionStore {
    conn: Connection,
}

/// Adapter `HistoryPersistence` untuk sesi: tiap panggilan menyimpan snapshot
/// penuh (prefix-compare di dalamnya) untuk satu sesi. Gagal → hanya dicatat
/// ke tracing; tidak menggagalkan turn (kernel best-effort).
///
/// `Connection` adalah `Send` tapi bukan `Sync`, jadi dibungkus `Mutex` agar
/// bisa dipakai dari `&self` (trait `HistoryPersistence`).
pub struct SessionPersistence {
    store: Arc<Mutex<SessionStore>>,
    session_id: String,
}

impl SessionPersistence {
    pub fn new(store: SessionStore, session_id: impl Into<String>) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            session_id: session_id.into(),
        }
    }

    /// Akses store (untuk snapshot akhir di main).
    pub fn store(&self) -> &Mutex<SessionStore> {
        &self.store
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Mode kerja tersimpan untuk sesi ini.
    pub fn mode(&self) -> Option<String> {
        let store = match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        store.session_mode(&self.session_id).ok().flatten()
    }

    /// Simpan mode kerja. Kegagalan tidak menggagalkan turn: mode tetap
    /// berlaku di sesi berjalan, hanya tidak bertahan setelah restart.
    pub fn set_mode(&self, mode: &str) {
        let store = match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = store.set_session_mode(&self.session_id, mode) {
            tracing::warn!(%error, "persist mode failed");
        }
    }
}

impl HistoryPersistence for SessionPersistence {
    fn save(&self, messages: &[Message]) {
        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = store.save_messages(&self.session_id, messages) {
            tracing::warn!(%error, "persist snapshot failed");
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub updated_at: u64,
    pub messages: u64,
}

impl SessionStore {
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        crate::config::ensure_gitignore(root);
        let dir = crate::config::minic_dir(root);
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("sessions.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                model      TEXT,
                cwd        TEXT
            );
            CREATE TABLE IF NOT EXISTS messages (
                session_id      TEXT NOT NULL,
                seq             INTEGER NOT NULL,
                role            TEXT NOT NULL,
                content_json    TEXT NOT NULL,
                tool_calls_json TEXT NOT NULL,
                tool_call_id    TEXT,
                name            TEXT,
                reasoning       TEXT,
                is_error        INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (session_id, seq)
            );",
        )?;
        // Migrasi tambahan: kolom `mode` menyimpan mode kerja (`discuss` /
        // `execute`) agar mode diskusi tidak diam-diam kembali ke eksekusi saat
        // sesi di-resume. `ALTER TABLE ... ADD COLUMN` gagal bila kolom sudah
        // ada, jadi kegagalan "duplicate column" diabaikan.
        let _ = self
            .conn
            .execute("ALTER TABLE sessions ADD COLUMN mode TEXT", []);
        Ok(())
    }

    /// Simpan mode kerja untuk sesi ini.
    pub fn set_session_mode(&self, id: &str, mode: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sessions SET mode = ?2 WHERE id = ?1",
            params![id, mode],
        )?;
        Ok(())
    }

    /// Mode kerja tersimpan untuk sesi ini, bila pernah dicatat.
    pub fn session_mode(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT mode FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn upsert_session(&self, id: &str, model: &str, cwd: &str) -> Result<(), StoreError> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at, model, cwd)
             VALUES (?1, ?2, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET updated_at = ?2, model = ?3, cwd = ?4",
            params![id, now as i64, model, cwd],
        )?;
        Ok(())
    }

    pub fn latest_session(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM sessions ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
             FROM sessions s ORDER BY s.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionInfo {
                id: row.get(0)?,
                updated_at: row.get::<_, i64>(1)? as u64,
                messages: row.get::<_, i64>(2)? as u64,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content_json, tool_calls_json, tool_call_id, name, reasoning, is_error
             FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;

        let mut messages = Vec::new();
        for row in rows {
            let (role, content_json, tool_calls_json, tool_call_id, name, reasoning, is_error) =
                row?;
            messages.push(Message {
                role: role_from(&role),
                content: serde_json::from_str::<Vec<Content>>(&content_json)?,
                tool_calls: serde_json::from_str::<Vec<ToolCall>>(&tool_calls_json)?,
                tool_call_id,
                name,
                reasoning,
                is_error: is_error != 0,
            });
        }
        Ok(messages)
    }

    /// Simpan `messages` untuk sesi; kembalikan jumlah pesan yang ditulis.
    // `&mut self`: dipakai dari `SessionPersistence` yang membungkus store
    // dalam `Mutex`, jadi mutasi aman.
    pub fn save_messages(
        &mut self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<usize, StoreError> {
        let stored = self.load_messages(session_id)?;
        let keep = common_prefix_len(&stored, messages);

        let tx = self.conn.transaction()?;
        if keep < stored.len() {
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1 AND seq >= ?2",
                params![session_id, keep as i64],
            )?;
        }
        let mut written = 0usize;
        for (offset, message) in messages[keep..].iter().enumerate() {
            let seq = (keep + offset) as i64;
            tx.execute(
                "INSERT INTO messages
                    (session_id, seq, role, content_json, tool_calls_json,
                     tool_call_id, name, reasoning, is_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    session_id,
                    seq,
                    role_str(message.role),
                    serde_json::to_string(&message.content)?,
                    serde_json::to_string(&message.tool_calls)?,
                    message.tool_call_id,
                    message.name,
                    message.reasoning,
                    if message.is_error { 1 } else { 0 },
                ],
            )?;
            written += 1;
        }
        tx.commit()?;
        Ok(written)
    }
}

fn common_prefix_len(a: &[Message], b: &[Message]) -> usize {
    let mut k = 0;
    while k < a.len() && k < b.len() && a[k] == b[k] {
        k += 1;
    }
    k
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from(role: &str) -> Role {
    match role {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
