//! Tokenizer: estimator token untuk budget kompaksi.
//!
//! Varian presisi (BPE, `cl100k_base`) tersedia lewat fitur `tiktoken`;
//! fallback heuristik (chars/4) tetap dipakai bila fitur nonaktif.

use crate::types::Message;

#[cfg(feature = "tiktoken")]
pub use tiktoken_impl::estimate_text;

#[cfg(not(feature = "tiktoken"))]
pub use heuristic_impl::estimate_text;

pub const DEFAULT_CHARS_PER_TOKEN: f64 = 4.0;

/// Estimasi token satu pesan (teks + overhead kecil per pesan + nama tool).
pub fn estimate_message(message: &Message) -> u64 {
    let mut total = estimate_text(&message.text()) + 4;
    for call in &message.tool_calls {
        total += estimate_text(&call.name);
        total += estimate_text(&call.arguments.to_string());
    }
    total
}

pub fn estimate_messages(messages: &[Message]) -> u64 {
    messages.iter().map(estimate_message).sum()
}

#[cfg(not(feature = "tiktoken"))]
mod heuristic_impl {
    use super::DEFAULT_CHARS_PER_TOKEN;

    pub fn estimate_text(text: &str) -> u64 {
        let chars = text.chars().count() as f64;
        (chars / DEFAULT_CHARS_PER_TOKEN).ceil().max(1.0) as u64
    }
}

#[cfg(feature = "tiktoken")]
mod tiktoken_impl {
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    use tiktoken_rs::CoreBPE;

    // `cl100k_base` vocab termuat sekali per proses (dibutuhkan sekitar
    // beberapa MB; mutex karena CoreBPE tidak Send).
    static BPE: Lazy<Mutex<CoreBPE>> = Lazy::new(|| {
        let bpe = tiktoken_rs::cl100k_base().expect("cl100k_base vocab built-in");
        Mutex::new(bpe)
    });

    pub fn estimate_text(text: &str) -> u64 {
        let tokens = BPE
            .lock()
            .map(|bpe| bpe.encode_with_special_tokens(text).len())
            .unwrap_or_else(|_| (text.chars().count() / 4).max(1));
        tokens.max(1) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_at_least_one_token() {
        assert_eq!(estimate_text(""), 1);
    }

    #[test]
    fn english_words_roughly_four_chars_per_token() {
        // Prosa normal: ~3.5–4.5 karakter per token untuk cl100k_base.
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let tokens = estimate_text(text);
        let chars = text.chars().count() as f64;
        let ratio = chars / tokens as f64;
        assert!(
            (2.5..=6.0).contains(&ratio),
            "ratio {ratio:.2} di luar rentang masuk akal (tokens={tokens})"
        );
    }

    #[test]
    fn punctuation_consumes_tokens() {
        let plain = estimate_text("hello world this is a test");
        let with_punct = estimate_text("hello world, this is a test!");
        assert!(with_punct >= plain);
    }
}
