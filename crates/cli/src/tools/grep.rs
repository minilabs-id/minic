//! `grep`: cari pola regex di isi berkas workspace.

use crate::policy::jail;
use crate::tools::common;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use regex::RegexBuilder;
use serde_json::{Value, json};
use walkdir::WalkDir;

const DEFAULT_MAX_RESULTS: usize = 100;
/// Batas atas baris konteks agar satu match tidak menghabiskan konteks model.
const MAX_CONTEXT_LINES: usize = 10;

/// Format satu match beserta konteksnya: baris match memakai `:`, konteks
/// memakai `-` (seperti `grep -C`) sehingga model bisa membedakan keduanya.
fn render_with_context(path: &str, lines: &[&str], index: usize, context: usize) -> String {
    let start = index.saturating_sub(context);
    let end = (index + context + 1).min(lines.len());
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(start) {
        let sep = if i == index { ':' } else { '-' };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{path}{sep}{}{sep}{}", i + 1, line.trim_end()));
    }
    out
}

pub struct Grep;

impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search workspace files with a regular expression. Returns path:line:text. Directories like .git/target/node_modules are skipped."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex syntax." },
                "path": { "type": "string", "description": "File or directory to search (default: workspace root)." },
                "glob": { "type": "string", "description": "Optional glob filter for file paths, e.g. **/*.rs" },
                "case_insensitive": { "type": "boolean", "default": false },
                "max_results": { "type": "integer", "default": 100 },
                "offset": { "type": "integer", "description": "Skip this many matches before returning results. Use to page past the first 100 hits." },
                "context": { "type": "integer", "description": "Show N lines before and after each match (like grep -C). Useful to see a function body instead of a single line." }
            },
            "required": ["pattern"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AgentError::InvalidRequest("grep: missing 'pattern'".to_string()))?;
            let case_insensitive = args
                .get("case_insensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let max_results = args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_MAX_RESULTS)
                .max(1);
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            // Saat memakai offset, pemindaian baru berhenti setelah
            // offset+max_results agar tidak menyusuri seluruh tree hanya untuk
            // membuang hasil awal.
            let stop_after = offset.saturating_add(max_results);
            let context = args
                .get("context")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(0)
                .min(MAX_CONTEXT_LINES);

            let search_root = match args.get("path").and_then(|v| v.as_str()) {
                Some(path) => jail::resolve(&ctx.cwd, path).map_err(AgentError::Permission)?,
                None => ctx.cwd.clone(),
            };
            let file_filter = match args.get("glob").and_then(|v| v.as_str()) {
                Some(glob) => Some(
                    globset::Glob::new(glob)
                        .map_err(|e| AgentError::InvalidRequest(format!("grep: bad glob: {e}")))?
                        .compile_matcher(),
                ),
                None => None,
            };

            let regex = RegexBuilder::new(pattern)
                .case_insensitive(case_insensitive)
                .build()
                .map_err(|e| AgentError::InvalidRequest(format!("grep: bad regex: {e}")))?;

            let mut matches: Vec<String> = Vec::new();
            let mut truncated = false;

            // Filter `.gitignore`: sumber kebenaran proyek, bukan daftar
            // hardcoded. Best-effort — tanpa `.gitignore` jatuh ke default.
            let ignore = common::WalkFilter::load(&ctx.cwd);
            'outer: for entry in WalkDir::new(&search_root)
                .into_iter()
                .filter_entry(|e| !ignore.is_ignored(e.path(), e.file_type().is_dir()))
                .flatten()
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if let Some(size) = common::file_size(path)
                    && size > common::MAX_SCAN_FILE_BYTES
                {
                    continue;
                }
                if let Some(filter) = &file_filter
                    && !filter.is_match(path)
                {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(path) else {
                    continue;
                };
                let shown = common::relative_display(&ctx.cwd, path);
                let lines: Vec<&str> = content.lines().collect();
                for (line_number, line) in lines.iter().enumerate() {
                    if regex.is_match(line) {
                        if context == 0 {
                            matches.push(format!(
                                "{shown}:{}:{}",
                                line_number + 1,
                                line.trim_end()
                            ));
                        } else {
                            matches.push(render_with_context(&shown, &lines, line_number, context));
                        }
                        if matches.len() >= stop_after {
                            truncated = true;
                            break 'outer;
                        }
                    }
                }
            }

            let skipped = matches.len().min(offset);
            let matches: Vec<String> = matches.into_iter().skip(offset).collect();

            let body = if matches.is_empty() {
                if skipped > 0 {
                    format!("no matches after skipping {offset}")
                } else {
                    "no matches".to_string()
                }
            } else {
                let mut body = matches.join("\n");
                if truncated {
                    body.push_str(&format!(
                        "\n… [truncated at {} matches; next page: offset={}]",
                        stop_after, stop_after
                    ));
                }
                body
            };
            Ok(ToolResult::ok(String::new(), "grep", body))
        })
    }
}
