//! Helper teks untuk tampilan (dipakai TUI dan renderer linear).
//!
//! Semua teks dari model/tool adalah input tak tepercaya: bersihkan escape
//! ANSI & karakter kontrol sebelum masuk layar. Lebar dihitung per kolom
//! (`unicode-width`), bukan per karakter.

use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Bersihkan teks untuk ditampilkan:
/// - buang sekuens ANSI (CSI/OSC) dan karakter kontrol C0/DEL,
/// - pertahankan `\n`, ubah `\r\n`/`\r` menjadi `\n`,
/// - tab menjadi 4 spasi (lebar kolom pasti).
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek().copied() {
                // CSI: ESC [ ... final byte 0x40..=0x7e
                Some('[') => {
                    let _ = chars.next();
                    while let Some(&next) = chars.peek() {
                        let _ = chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... BEL atau ST (ESC \)
                Some(']') => {
                    let _ = chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            let _ = chars.next();
                            break;
                        }
                    }
                }
                // Escape lain: lewati satu karakter.
                Some(_) => {
                    let _ = chars.next();
                }
                None => {}
            },
            '\r' => {
                // `\r\n` → satu newline; `\r` tunggal juga newline.
                if chars.peek() == Some(&'\n') {
                    let _ = chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Rangkum argumen tool menjadi satu baris pendek (untuk tool card/ledger).
pub fn summarize_tool(name: &str, args: &Value) -> String {
    let raw = match name {
        "bash" => args.get("command").and_then(Value::as_str).unwrap_or(""),
        "read_file" | "write_file" | "edit" => {
            args.get("path").and_then(Value::as_str).unwrap_or("")
        }
        "grep" => {
            let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            let path = args.get("path").and_then(Value::as_str).unwrap_or("");
            if path.is_empty() {
                pattern
            } else {
                return truncate(&format!("{pattern}  {path}"), 72);
            }
        }
        "glob" => args.get("pattern").and_then(Value::as_str).unwrap_or(""),
        "delegate_task" => {
            let count = args
                .get("tasks")
                .and_then(Value::as_array)
                .map(|tasks| tasks.len())
                .unwrap_or(0);
            return format!("{count} task(s)");
        }
        "mcp_call" => {
            let server = args.get("server").and_then(Value::as_str).unwrap_or("");
            let tool = args.get("tool").and_then(Value::as_str).unwrap_or("");
            return format!("{server}.{tool}");
        }
        _ => "",
    };
    let line = sanitize(raw)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    truncate(&line, 72)
}

/// Satu baris cuplikan hasil tool (dipakai saat error).
pub fn preview_line(text: &str) -> String {
    let line = sanitize(text)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    truncate(&line, 120)
}

/// Potong berdasarkan lebar kolom, tambahkan `…` bila terpotong.
pub fn truncate(text: &str, max_cols: usize) -> String {
    if display_width(text) <= max_cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if width + w > max_cols.saturating_sub(1) {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('…');
    out
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Format durasi ringkas: `12ms`, `1.4s`, `2m03s`.
pub fn format_duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1000..=59_999 => format!("{:.1}s", ms as f64 / 1000.0),
        _ => {
            let total = ms / 1000;
            format!("{}m{:02}s", total / 60, total % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_control_chars() {
        let cleaned = sanitize("\u{1b}[31mred\u{1b}[0m\u{7}!\r\nnext\tcell");
        assert_eq!(cleaned, "red!\nnext    cell");
    }

    #[test]
    fn truncates_by_display_width() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        // CJK = 2 kolom: hanya satu karakter yang muat.
        assert_eq!(truncate("日本語", 4), "日…");
    }

    #[test]
    fn tool_summaries_are_compact() {
        let args = serde_json::json!({"command": "cargo test\n--all"});
        assert_eq!(summarize_tool("bash", &args), "cargo test");
        let args = serde_json::json!({"tasks": ["a", "b", "c"]});
        assert_eq!(summarize_tool("delegate_task", &args), "3 task(s)");
    }
}
