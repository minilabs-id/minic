//! `write_file`: tulis berkas baru / timpa, secara atomik.

use crate::policy::jail;
use crate::tools::common;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};

pub struct WriteFile;

impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a UTF-8 text file in the workspace. Parent directories are created."
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let raw = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                AgentError::InvalidRequest("write_file: missing 'path'".to_string())
            })?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    AgentError::InvalidRequest("write_file: missing 'content'".to_string())
                })?;
            let path = jail::resolve(&ctx.cwd, raw).map_err(AgentError::Permission)?;
            // Snapshot best-effort sebelum menimpa (untuk `--undo`).
            let _ = crate::checkpoints::snapshot(&ctx.cwd, &path);
            common::atomic_write(&path, content)
                .map_err(|e| AgentError::Tool(format!("write_file {}: {e}", path.display())))?;
            let shown = common::relative_display(&ctx.cwd, &path);
            Ok(ToolResult::ok(
                String::new(),
                "write_file",
                format!("wrote {} bytes to {shown}", content.len()),
            ))
        })
    }
}
