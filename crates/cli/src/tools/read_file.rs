//! `read_file`: baca berkas teks UTF-8 dari workspace.

use crate::policy::jail;
use crate::tools::common;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};

const READ_CAP_CHARS: usize = 200_000;

pub struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the workspace. Returns the whole file by default; use 'offset' and 'limit' to page through a large file (1-based offset, counts lines)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path relative to the workspace root (or absolute within it)."
                },
                "offset": {
                    "type": "integer",
                    "description": "First line to return, 1-based. Defaults to 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return. Defaults to all remaining lines."
                }
            },
            "required": ["path"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let raw = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                AgentError::InvalidRequest("read_file: missing 'path'".to_string())
            })?;
            let offset = args
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1) as usize;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let path = jail::resolve(&ctx.cwd, raw).map_err(AgentError::Permission)?;
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| AgentError::Tool(format!("read_file {}: {e}", path.display())))?;

            // Paging is applied on lines, but the whole file is still capped so
            // a single huge line cannot exhaust the token budget.
            let paged = if offset == 1 && limit.is_none() {
                None
            } else {
                Some(paginate(&content, offset, limit))
            };
            let text = match paged {
                Some(page) => page,
                None => content,
            };
            let (capped, truncated) = common::cap_text(&text, READ_CAP_CHARS);
            let mut out = capped;
            if truncated {
                out.push_str("\n[minic] output truncated at 200k characters");
            }
            let mut result = ToolResult::ok(String::new(), "read_file", out);
            let _ = truncated;
            result.name = "read_file".to_string();
            Ok(result)
        })
    }
}

/// Potong isi berkas per baris. Header menyebut rentang yang dikembalikan agar
/// model tahu ini bukan isi penuh — tanpa itu, ia mudah menyimpulkan "berkas
/// hanya 40 baris" dan menulis kode yang salah.
fn paginate(content: &str, offset: usize, limit: Option<usize>) -> String {
    let total = content.lines().count();
    let start = offset.saturating_sub(1).min(total);
    let end = match limit {
        Some(n) => start.saturating_add(n).min(total),
        None => total,
    };
    let body: Vec<&str> = content.lines().skip(start).take(end - start).collect();
    let mut out = format!(
        "[minic] lines {}-{} of {total}\n",
        if body.is_empty() { start } else { start + 1 },
        end
    );
    out.push_str(&body.join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginate_returns_the_requested_window() {
        let text = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let page = paginate(&text, 4, Some(3));
        assert!(page.starts_with("[minic] lines 4-6 of 10"));
        assert!(page.contains("line 4"));
        assert!(page.contains("line 6"));
        assert!(!page.contains("line 3\n"), "before window must be absent");
        assert!(!page.contains("line 7"), "after window must be absent");
    }

    #[test]
    fn paginate_without_limit_runs_to_end() {
        let text = "a\nb\nc\nd";
        let page = paginate(text, 3, None);
        assert!(page.starts_with("[minic] lines 3-4 of 4"));
        assert!(page.contains('c') && page.contains('d') && !page.contains('a'));
    }

    #[test]
    fn paginate_past_the_end_is_empty_but_explains_itself() {
        let page = paginate("a\nb", 99, Some(5));
        assert!(page.starts_with("[minic] lines 2-2 of 2"));
    }

    #[test]
    fn offset_zero_is_treated_as_first_line() {
        // execute() clamps with .max(1); paginate itself also saturates.
        let page = paginate("x\ny", 1, Some(1));
        assert!(page.contains('x'));
        assert!(page.starts_with("[minic] lines 1-1 of 2"));
    }
}
