//! `glob`: temukan berkas berdasarkan pola glob.

use crate::policy::jail;
use crate::tools::common;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};
use std::path::Path;
use walkdir::WalkDir;

const DEFAULT_MAX_RESULTS: usize = 200;

pub struct Glob;

impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find workspace files matching a glob pattern (e.g. **/*.ts). Directories like .git/target/node_modules are skipped."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. src/**/*.rs" },
                "path": { "type": "string", "description": "Directory to search (default: workspace root)." },
                "max_results": { "type": "integer", "default": 200 }
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
                .ok_or_else(|| AgentError::InvalidRequest("glob: missing 'pattern'".to_string()))?;
            let max_results = args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_MAX_RESULTS);

            let search_root = match args.get("path").and_then(|v| v.as_str()) {
                Some(path) => jail::resolve(&ctx.cwd, path).map_err(AgentError::Permission)?,
                None => ctx.cwd.clone(),
            };
            let matcher = globset::Glob::new(pattern)
                .map_err(|e| AgentError::InvalidRequest(format!("glob: bad pattern: {e}")))?
                .compile_matcher();

            let mut found: Vec<String> = Vec::new();
            let mut truncated = false;

            for entry in WalkDir::new(&search_root)
                .into_iter()
                .filter_entry(|e| !common::skip_dir(e))
                .flatten()
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let shown = common::relative_display(&ctx.cwd, entry.path());
                if matcher.is_match(Path::new(&shown)) {
                    found.push(shown);
                    if found.len() >= max_results {
                        truncated = true;
                        break;
                    }
                }
            }

            found.sort();
            let body = if found.is_empty() {
                "no matches".to_string()
            } else {
                let mut body = found.join("\n");
                if truncated {
                    body.push_str(&format!("\n… [truncated at {max_results} files]"));
                }
                body
            };
            Ok(ToolResult::ok(String::new(), "glob", body))
        })
    }
}
