//! Kompaksi konteks mekanis (lossy, deterministik, tanpa LLM).
//!
//! Saat history mendekati budget konteks, pesan lama dilipat menjadi satu
//! ringkasan `system` sementara pesan terbaru dipertahankan. Ini **lossy**:
//! detail lama tidak bisa dipulihkan — state penting harus di task/file.
//!
//! Keamanan: jendela yang dipertahankan tidak boleh dimulai dengan pesan
//! `Tool` yatim (tanpa `assistant` pemanggilnya di depan), karena provider bisa
//! menolak pasangan tool_call/tool_result yang tidak lengkap.

use crate::types::{Message, Role};

pub const DEFAULT_KEEP_RECENT: usize = 8;

/// Batas token efektif = 80% dari context window (sisakan ruang untuk output).
pub fn budget_tokens(window: u32) -> u64 {
    (window as u64 * 8) / 10
}

/// Lipat pesan lama. Kembalikan `(history_baru, jumlah_pesan_dilipat)`.
pub fn mechanical_compact(messages: &[Message], keep_recent: usize) -> (Vec<Message>, usize) {
    let keep = keep_recent.max(1);
    if messages.len() <= keep {
        return (messages.to_vec(), 0);
    }

    // Pesan system di awal selalu dipertahankan.
    let system_count = messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .count();
    let body = &messages[system_count..];
    if body.len() <= keep {
        return (messages.to_vec(), 0);
    }

    let mut cut = body.len() - keep;
    // Jangan mulai jendela dengan Tool yatim → geser batas ke kiri.
    while cut > 0 && body[cut].role == Role::Tool {
        cut -= 1;
    }
    if cut == 0 {
        return (messages.to_vec(), 0);
    }

    let folded = &body[..cut];
    let kept = &body[cut..];

    let mut summary = format!(
        "[compaction] {} earlier message(s) elided to save context.",
        folded.len()
    );
    for message in folded.iter().take(20) {
        let head: String = message.text().chars().take(200).collect();
        if !head.is_empty() {
            summary.push_str(&format!("\n- {}: {}", role_label(message.role), head));
        }
    }

    let mut out: Vec<Message> = messages[..system_count]
        .iter()
        .filter(|message| !is_compaction_summary(message))
        .cloned()
        .collect();
    out.push(Message::system(summary));
    out.extend_from_slice(kept);
    (out, folded.len())
}

fn is_compaction_summary(message: &Message) -> bool {
    message.role == Role::System && message.text().starts_with("[compaction]")
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}
