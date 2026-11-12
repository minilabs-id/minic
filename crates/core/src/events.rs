//! Event agen. Dipakai host untuk render; tidak menyimpan state.

/// Bentuk keputusan izin yang dilaporkan ke host (lihat `AgentEvent::ToolDecided`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// Tool dieksekusi.
    Allow,
    /// Tool ditolak; tidak dieksekusi.
    Deny,
    /// Tool dieksekusi karena diizinkan permanen untuk sesi ini (pengguna
    /// memilih "always").
    Always,
}

impl DecisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionKind::Allow => "allow",
            DecisionKind::Deny => "deny",
            DecisionKind::Always => "always",
        }
    }
}
#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStarted {
        turn: u32,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
        /// Argumen mentah tool (host merangkumnya untuk ditampilkan).
        arguments: serde_json::Value,
    },
    /// Keputusan izin final untuk satu tool call, dipancarkan setelah
    /// `Ask` diselesaikan dan sebelum tool dieksekusi.
    ///
    /// Murni observability (audit/trace): tidak mengubah alur agent. `decision`
    /// adalah salah satu dari `allow`, `deny`, `always` (diizinkan permanen
    /// untuk sesi ini). `reason` tidak memuat argumen mentah bila argumen itu
    /// bisa berisi secret — host yang memutuskan (lihat telemetry).
    ToolDecided {
        id: String,
        name: String,
        decision: DecisionKind,
        reason: String,
    },
    ToolCallFinished {
        id: String,
        name: String,
        is_error: bool,
        /// Durasi eksekusi tool (0 untuk jalur yang tidak dieksekusi).
        duration_ms: u64,
        /// Cuplikan hasil (satu baris, sudah dipotong) untuk status/ledger.
        result_preview: String,
        /// Hasil lengkap (dipotong longgar, sudah dibersihkan) untuk panel
        /// detail di TUI. Tidak pernah masuk ledger atau `--json` secara default
        /// agar stream tetap ringkas.
        result_detail: String,
    },
    TurnFinished {
        turn: u32,
    },
    /// Peringatan non-fatal dari host (mis. post-hook gagal). Ditampilkan sebagai
    /// catatan di transcript supaya tidak hilang di stderr saat TUI memakai
    /// alternate screen.
    Note {
        text: String,
    },
    AgentFinished {
        steps: u32,
    },
    ContextCompacted {
        elided: usize,
    },
    Error {
        message: String,
    },
}
