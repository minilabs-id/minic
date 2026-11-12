//! Dekoder Server-Sent Events (SSE) yang aman terhadap batas chunk.
//!
//! Dibaca sebagai byte dan dipotong **hanya pada `\n`**, sehingga urutan
//! multibyte UTF-8 yang terbelah antar chunk tidak pernah rusak (pelajaran
//! baseline: fragmentasi ANSI/UTF-8 di batas chunk pernah jadi bug nyata).

#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Beri byte baru; kembalikan payload `data:` yang sudah lengkap (satu event
    /// SSE dipisah baris kosong). Multi-baris `data:` digabung dengan `\n`.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // buang '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }

            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = strip_prefix(&line, b"data:") {
                let rest = strip_prefix(rest, b" ").unwrap_or(rest);
                self.data.push(String::from_utf8_lossy(rest).into_owned());
            }
            // field lain (`event:`, `id:`, `retry:`) diabaikan.
        }

        events
    }
}

fn strip_prefix<'a>(haystack: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    haystack.strip_prefix(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_on_blank_line() {
        let mut d = SseDecoder::new();
        let ev = d.push(b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n");
        assert_eq!(ev, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
    }

    #[test]
    fn survives_split_across_chunks() {
        let mut d = SseDecoder::new();
        assert!(d.push(b"data: hel").is_empty());
        assert!(d.push(b"lo\n").is_empty());
        let ev = d.push(b"\n");
        assert_eq!(ev, vec!["hello".to_string()]);
    }

    #[test]
    fn handles_crlf() {
        let mut d = SseDecoder::new();
        let ev = d.push(b"data: x\r\n\r\n");
        assert_eq!(ev, vec!["x".to_string()]);
    }

    #[test]
    fn multibyte_split_is_not_corrupted() {
        // "é" = 0xC3 0xA9, dibelah antar chunk.
        let mut d = SseDecoder::new();
        d.push(b"data: \xc3");
        let ev = d.push(b"\xa9\n\n");
        assert_eq!(ev, vec!["é".to_string()]);
    }
}
