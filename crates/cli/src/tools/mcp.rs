//! Tool MCP: `mcp_list` dan `mcp_call`.

use crate::mcp::McpManager;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct McpCall {
    manager: Arc<McpManager>,
}

impl McpCall {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpCall {
    fn name(&self) -> &str {
        "mcp_call"
    }

    fn description(&self) -> &str {
        "Call a tool exposed by a configured MCP server (see mcp_list)."
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": { "type": "string" },
                "tool": { "type": "string" },
                "arguments": { "type": "object" }
            },
            "required": ["server", "tool"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let server = args.get("server").and_then(Value::as_str).ok_or_else(|| {
                AgentError::InvalidRequest("mcp_call: missing 'server'".to_string())
            })?;
            let tool = args.get("tool").and_then(Value::as_str).ok_or_else(|| {
                AgentError::InvalidRequest("mcp_call: missing 'tool'".to_string())
            })?;
            let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));

            let Some(client) = self.manager.get(server) else {
                let available = self.manager.names().join(", ");
                return Ok(ToolResult::err(
                    String::new(),
                    "mcp_call",
                    format!("unknown MCP server '{server}' (available: {available})"),
                ));
            };

            match client.call_tool(tool, arguments).await {
                Ok((text, is_error)) => {
                    let mut result = ToolResult::ok(String::new(), "mcp_call", text);
                    result.is_error = is_error;
                    Ok(result)
                }
                Err(error) => Ok(ToolResult::err(String::new(), "mcp_call", error)),
            }
        })
    }
}

pub struct McpList {
    manager: Arc<McpManager>,
}

impl McpList {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpList {
    fn name(&self) -> &str {
        "mcp_list"
    }

    fn description(&self) -> &str {
        "List configured MCP servers and the tools they expose."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn execute<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            if self.manager.is_empty() {
                return Ok(ToolResult::ok(
                    String::new(),
                    "mcp_list",
                    "no MCP servers configured (.minic/mcp.json)".to_string(),
                ));
            }

            let mut out = String::new();
            for name in self.manager.names() {
                out.push_str(&format!("{name}:\n"));
                if let Some(client) = self.manager.get(&name) {
                    match client.list_tools().await {
                        Ok(tools) => {
                            for tool in tools {
                                let tool_name =
                                    tool.get("name").and_then(Value::as_str).unwrap_or("?");
                                let desc = tool
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                out.push_str(&format!("  - {tool_name}: {desc}\n"));
                            }
                        }
                        Err(error) => out.push_str(&format!("  (error: {error})\n")),
                    }
                }
            }
            Ok(ToolResult::ok(
                String::new(),
                "mcp_list",
                out.trim_end().to_string(),
            ))
        })
    }
}
