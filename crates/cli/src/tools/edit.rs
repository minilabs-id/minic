//! `edit`: ganti `old_string` → `new_string` di sebuah berkas.
//!
//! Matcher bertingkat (dari paling ketat ke paling toleran):
//! 1. **exact** — cocok byte-per-byte.
//! 2. **line-ending-insensitive** — CRLF vs LF tidak masalah.
//! 3. **whitespace-tolerant** — spasi/tab di ujung baris diabaikan.
//!
//! Aturan keunikan: kecuali `replace_all=true`, `old_string` harus cocok
//! **tepat satu kali**. Bila ambigu, tool menolak (tidak menebak).

use crate::policy::jail;
use crate::tools::common;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};

pub struct Edit;

impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a workspace file. Tolerates CRLF and trailing whitespace differences. Fails if the match is ambiguous unless replace_all is true."
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean", "default": false }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AgentError::InvalidRequest("edit: missing 'path'".to_string()))?;
            let old = args
                .get("old_string")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    AgentError::InvalidRequest("edit: missing 'old_string'".to_string())
                })?;
            let new = args
                .get("new_string")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    AgentError::InvalidRequest("edit: missing 'new_string'".to_string())
                })?;
            let replace_all = args
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let path = jail::resolve(&ctx.cwd, raw).map_err(AgentError::Permission)?;
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| AgentError::Tool(format!("edit {}: {e}", path.display())))?;

            let (updated, replacements) =
                apply_edit(&content, old, new, replace_all).map_err(AgentError::Tool)?;
            // Snapshot best-effort sebelum menulis (untuk `--undo`).
            let _ = crate::checkpoints::snapshot(&ctx.cwd, &path);
            common::atomic_write(&path, &updated)
                .map_err(|e| AgentError::Tool(format!("edit {}: {e}", path.display())))?;

            let shown = common::relative_display(&ctx.cwd, &path);
            Ok(ToolResult::ok(
                String::new(),
                "edit",
                format!("replaced {replacements} occurrence(s) in {shown}"),
            ))
        })
    }
}

/// Terapkan edit; kembalikan `(isi_baru, jumlah_penggantian)`.
pub fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old.is_empty() {
        return Err("old_string must not be empty".to_string());
    }
    if old == new {
        return Err("old_string and new_string are identical".to_string());
    }

    // Tier 1: exact.
    let exact = content.matches(old).count();
    if exact == 1 {
        return Ok((content.replacen(old, new, 1), 1));
    }
    if exact > 1 {
        return if replace_all {
            Ok((content.replace(old, new), exact))
        } else {
            Err(format!(
                "old_string matches {exact} times (exact); make it unique or set replace_all"
            ))
        };
    }

    // Tier 2: line-ending-insensitive.
    let normalized = content.replace("\r\n", "\n");
    let old_n = old.replace("\r\n", "\n");
    let new_n = new.replace("\r\n", "\n");
    let count_n = normalized.matches(&old_n).count();
    if count_n == 1 {
        let replaced = normalized.replacen(&old_n, &new_n, 1);
        return Ok((restore_line_endings(content, replaced), 1));
    }
    if count_n > 1 {
        return if replace_all {
            let replaced = normalized.replace(&old_n, &new_n);
            Ok((restore_line_endings(content, replaced), count_n))
        } else {
            Err(format!(
                "old_string matches {count_n} times (line-ending-insensitive); make it unique or set replace_all"
            ))
        };
    }

    // Tier 3: whitespace-tolerant (abaikan spasi/tab di ujung baris).
    if let Some((updated, count)) = apply_trimmed(content, old, new, replace_all)? {
        return Ok((updated, count));
    }

    Err(
        "old_string not found (tried exact, line-ending-insensitive, and whitespace-tolerant)"
            .to_string(),
    )
}

fn restore_line_endings(original: &str, replaced: String) -> String {
    if original.contains("\r\n") {
        replaced.replace('\n', "\r\n")
    } else {
        replaced
    }
}

fn apply_trimmed(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<Option<(String, usize)>, String> {
    let old_lines: Vec<&str> = old.lines().map(|l| l.trim_end()).collect();
    if old_lines.is_empty() {
        return Ok(None);
    }
    let content_lines: Vec<&str> = content.lines().map(|l| l.trim_end()).collect();
    let n = old_lines.len();
    if content_lines.len() < n {
        return Ok(None);
    }

    let mut starts = Vec::new();
    for i in 0..=content_lines.len() - n {
        if content_lines[i..i + n] == old_lines[..] {
            starts.push(i);
        }
    }
    if starts.is_empty() {
        return Ok(None);
    }
    if starts.len() > 1 && !replace_all {
        return Err(format!(
            "old_string matches {} times (whitespace-tolerant); make it unique or set replace_all",
            starts.len()
        ));
    }

    let offsets = line_starts(content);
    let line_start = |line: usize| offsets.get(line).copied().unwrap_or(content.len());
    let line_span_end = |line: usize| offsets.get(line + n).copied().unwrap_or(content.len());

    if replace_all {
        let mut out = String::new();
        let mut cursor = 0usize;
        for &start in &starts {
            let from = line_start(start);
            let to = line_span_end(start);
            out.push_str(&content[cursor..from]);
            out.push_str(new);
            cursor = to;
        }
        out.push_str(&content[cursor..]);
        Ok(Some((out, starts.len())))
    } else {
        let start = starts[0];
        let from = line_start(start);
        let to = line_span_end(start);
        let mut out = String::new();
        out.push_str(&content[..from]);
        out.push_str(new);
        out.push_str(&content[to..]);
        Ok(Some((out, 1)))
    }
}

/// Byte offset awal setiap baris.
fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_replacement() {
        let (out, n) = apply_edit("hello world", "world", "there", false).unwrap();
        assert_eq!(out, "hello there");
        assert_eq!(n, 1);
    }

    #[test]
    fn ambiguous_is_rejected() {
        let err = apply_edit("a a a", "a", "b", false).unwrap_err();
        assert!(err.contains("matches 3 times"));
    }

    #[test]
    fn replace_all_works() {
        let (out, n) = apply_edit("a a a", "a", "b", true).unwrap();
        assert_eq!(out, "b b b");
        assert_eq!(n, 3);
    }

    #[test]
    fn crlf_tolerant_match() {
        let content = "line1\r\nline2\r\nline3";
        let (out, _) = apply_edit(content, "line2", "LINE2", false).unwrap();
        assert_eq!(out, "line1\r\nLINE2\r\nline3");
    }

    #[test]
    fn trailing_whitespace_tolerant() {
        let content = "fn main() {   \n    println!();\n}";
        let old = "fn main() {\n    println!();\n}";
        let (out, n) = apply_edit(content, old, "fn main() {}", false).unwrap();
        assert_eq!(out, "fn main() {}");
        assert_eq!(n, 1);
    }

    #[test]
    fn empty_old_is_rejected() {
        assert!(apply_edit("x", "", "y", false).is_err());
    }
}
