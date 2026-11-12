//! `lsp_diagnostics`: ambil diagnostik bahasa untuk satu berkas.

use crate::lsp;
use crate::policy::jail;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};

pub struct LspDiagnostics;

impl Tool for LspDiagnostics {
    fn name(&self) -> &str {
        "lsp_diagnostics"
    }

    fn description(&self) -> &str {
        "Return language-server diagnostics for a workspace file (configured in .minic/lsp.json by extension)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to check." }
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
                AgentError::InvalidRequest("lsp_diagnostics: missing 'path'".to_string())
            })?;
            let path = jail::resolve(&ctx.cwd, raw).map_err(AgentError::Permission)?;

            match lsp::diagnostics(&ctx.cwd, &path).await {
                Ok(diagnostics) if diagnostics.is_empty() => Ok(ToolResult::ok(
                    String::new(),
                    "lsp_diagnostics",
                    "no diagnostics".to_string(),
                )),
                Ok(diagnostics) => {
                    let mut out = String::new();
                    for diagnostic in diagnostics {
                        out.push_str(&format!(
                            "line {} [{}] {}\n",
                            diagnostic.line, diagnostic.severity, diagnostic.message
                        ));
                    }
                    Ok(ToolResult::ok(
                        String::new(),
                        "lsp_diagnostics",
                        out.trim_end().to_string(),
                    ))
                }
                Err(error) => Ok(ToolResult::err(String::new(), "lsp_diagnostics", error)),
            }
        })
    }
}
