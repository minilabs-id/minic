//! Approval interaktif untuk mode `ask`.
//!
//! Dua jalur:
//! - **TUI**: permintaan dikirim lewat channel ke driver TUI dan dijawab dari
//!   modal (tidak pernah membaca stdin langsung — stdin dipakai event loop).
//! - **Linear**: prompt di terminal seperti biasa.
//!
//! Aturan fail-closed: bila tidak ada penjawab (non-TTY, channel tertutup),
//! approval **selalu ditolak**. Tidak ada jalan diam-diam untuk menyetujui.

use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};
use tokio::sync::oneshot;

/// Kategori izin. Saat pengguna memilih "izinkan selalu", kita menyimpan
/// *scope* (kategori), bukan sekadar nama tool: `a` pada satu `read_file` tidak
/// boleh berarti "izinkan semua tool". Tapi `a` pada `bash` juga tidak
/// seharusnya berarti "izinkan setiap perintah shell selamanya" tanpa disadari
/// — kategori membuat cakupannya terlihat di modal dan di transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Tool yang hanya membaca workspace (`read_file`, `grep`, `glob`).
    Read,
    /// Menulis/mengubah berkas (`write_file`, `edit`).
    FileWrite,
    /// Menjalankan shell (`bash`).
    Shell,
    /// Memanggil server MCP (`mcp_call`, `mcp_list`). Dipisah dari `Other`
    /// supaya "izinkan seluruh kategori" pada satu MCP call tidak diam-diam
    /// mengizinkan delegasi subagent (dan sebaliknya).
    Mcp,
    /// Delegasi ke subagent (`delegate_task`).
    Delegate,
    /// Tool yang tidak dikenali. **Sengaja tidak punya cakupan kategori**:
    /// mengizinkan kategori ini berarti mengizinkan setiap tool baru yang
    /// belum dipetakan, yang terlalu luas untuk satu tombol.
    Other,
}

impl Category {
    pub fn of_tool(tool: &str) -> Self {
        match tool {
            "read_file" | "grep" | "glob" | "lsp_diagnostics" => Category::Read,
            "write_file" | "edit" => Category::FileWrite,
            "bash" => Category::Shell,
            "mcp_call" | "mcp_list" => Category::Mcp,
            "delegate_task" => Category::Delegate,
            _ => Category::Other,
        }
    }

    /// Apakah kategori ini boleh diberikan lewat tombol "izinkan seluruh
    /// kategori" (`c`). `Other` tidak boleh: isinya adalah tool yang belum
    /// dipetakan, jadi cakupannya tidak dapat dijelaskan ke pengguna.
    pub fn is_grantable(self) -> bool {
        !matches!(self, Category::Other)
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Read => "read",
            Category::FileWrite => "file-write",
            Category::Shell => "shell",
            Category::Mcp => "mcp",
            Category::Delegate => "delegate",
            Category::Other => "other",
        }
    }
}

/// Scope izin permanen untuk sesi ini.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Satu tool dengan nama tertentu (perilaku lama `a`).
    Tool(String),
    /// Seluruh kategori (mis. semua `read_file`/`grep`/`glob`).
    Category(Category),
}

/// Jawaban pengguna atas satu permintaan approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    /// Izinkan tool ini sekali.
    AllowOnce,
    /// Izinkan semua pemanggilan tool dengan nama yang sama pada sesi ini.
    AllowAlways,
    /// Izinkan semua tool dalam kategori yang sama pada sesi ini
    /// (mis. semua tool baca, atau semua `bash`).
    AllowCategory(Category),
    Deny,
}

/// Permintaan approval yang ditampilkan TUI. `reply` wajib dijawab; bila
/// receiver-nya hilang, policy menolak (fail-closed).
pub struct ApprovalRequest {
    pub tool: String,
    pub summary: String,
    pub reason: String,
    /// Kategori tool, ditampilkan di modal agar cakupan `a` jelas.
    pub category: Category,
    /// Argumen mentah (sudah di-sanitize) untuk tombol `?` di modal.
    pub details: String,
    pub reply: oneshot::Sender<ApprovalChoice>,
}

impl std::fmt::Debug for ApprovalRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalRequest")
            .field("tool", &self.tool)
            .field("summary", &self.summary)
            .field("reason", &self.reason)
            .field("category", &self.category)
            .field("details", &self.details)
            .finish_non_exhaustive()
    }
}

/// Apakah sesi interaktif (stdin & stdout keduanya TTY)?
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Rangkum argumen tool untuk ditampilkan di prompt/modal. Ringkas dan aman:
/// argumen mentah bisa sangat panjang atau berisi escape sequence.
pub fn summarize_args(tool: &str, args: &Value) -> String {
    if args.is_null() || args.as_object().is_some_and(|object| object.is_empty()) {
        return String::new();
    }
    let summary = crate::render::text::summarize_tool(tool, args);
    if !summary.is_empty() {
        return summary;
    }
    let raw = args.to_string();
    crate::render::text::truncate(&crate::render::text::sanitize(&raw), 120)
}

/// Argumen lengkap (multi-baris, sudah di-sanitize) untuk tampilan `?` di modal.
/// Dibatasi agar satu tool call tidak bisa memenuhi memori/layar.
pub fn detail_args(args: &Value) -> String {
    const MAX_CHARS: usize = 4_000;
    let pretty = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    let cleaned = crate::render::text::sanitize(&pretty);
    if cleaned.chars().count() <= MAX_CHARS {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(MAX_CHARS).collect();
        out.push_str("\n… (truncated)");
        out
    }
}

/// Tanyakan persetujuan ke pengguna di terminal.
///
/// `a` (atau `all`) mengizinkan tool ini untuk sisa sesi; `c` mengizinkan
/// seluruh kategorinya. Mengembalikan `Deny` untuk apa pun selain itu, termasuk
/// non-TTY dan EOF (fail-closed).
pub fn prompt_ask(tool: &str, args: &Value, reason: &str) -> ApprovalChoice {
    if !is_interactive() {
        return ApprovalChoice::Deny;
    }

    let summary = summarize_args(tool, args);
    let category = Category::of_tool(tool);
    if summary.is_empty() {
        eprint!(
            "\n  approve {tool}\n  ({reason}) [y/N/a=this tool/c=all {category}] ",
            category = category.label()
        );
    } else {
        eprint!(
            "\n  approve {tool} {summary}\n  ({reason}) [y/N/a=this tool/c=all {category}] ",
            category = category.label()
        );
    }
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return ApprovalChoice::Deny;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalChoice::AllowOnce,
        "a" | "all" => ApprovalChoice::AllowAlways,
        "c" => ApprovalChoice::AllowCategory(category),
        _ => ApprovalChoice::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_args_is_compact() {
        assert_eq!(
            summarize_args("read_file", &serde_json::json!({"path": "a.rs"})),
            "a.rs"
        );
        assert_eq!(summarize_args("unknown", &serde_json::json!({})), "");
        assert_eq!(summarize_args("unknown", &serde_json::Value::Null), "");
    }

    #[test]
    fn categories_map_tools() {
        assert_eq!(Category::of_tool("read_file"), Category::Read);
        assert_eq!(Category::of_tool("grep"), Category::Read);
        assert_eq!(Category::of_tool("edit"), Category::FileWrite);
        assert_eq!(Category::of_tool("bash"), Category::Shell);
        assert_eq!(Category::of_tool("delegate_task"), Category::Delegate);
        assert_eq!(Category::of_tool("mcp_call"), Category::Mcp);
        assert_eq!(Category::of_tool("mcp_list"), Category::Mcp);
        assert_eq!(Category::of_tool("some_future_tool"), Category::Other);
    }

    /// `ab` pada satu MCP call tidak boleh ikut mengizinkan delegasi subagent:
    /// keduanya dulu berbagi `Other`.
    #[test]
    fn mcp_and_delegate_do_not_share_a_category() {
        assert_ne!(
            Category::of_tool("mcp_call"),
            Category::of_tool("delegate_task")
        );
        assert_ne!(Category::of_tool("mcp_call"), Category::of_tool("bash"));
    }

    /// Kategori yang belum dipetakan tidak boleh diberikan lewat satu tombol.
    #[test]
    fn unknown_tools_are_not_grantable_as_a_category() {
        assert!(!Category::Other.is_grantable());
        for tool in ["read_file", "edit", "bash", "mcp_call", "delegate_task"] {
            assert!(
                Category::of_tool(tool).is_grantable(),
                "{tool} harus bisa diberikan per kategori"
            );
        }
    }

    #[test]
    fn detail_args_is_pretty_and_sanitized() {
        let details = detail_args(&serde_json::json!({"command": "rm -rf /\u{1b}[31m"}));
        assert!(details.contains("rm -rf"), "details: {details}");
        assert!(!details.contains('\u{1b}'), "ANSI harus dibersihkan");
        assert!(details.contains('\n'), "pretty-print multi-baris");
    }

    #[test]
    fn detail_args_is_capped() {
        let big = "x".repeat(10_000);
        let details = detail_args(&serde_json::json!({ "content": big }));
        assert!(details.chars().count() <= 4_100, "harus dipotong");
        assert!(details.contains("truncated"));
    }
}
