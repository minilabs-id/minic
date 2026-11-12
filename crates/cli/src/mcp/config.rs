//! Konfigurasi server MCP: `<root>/.minic/mcp.json`.
//!
//! ```json
//! {
//!   "servers": {
//!     "demo": { "command": "node", "args": ["server.js"], "env": {} }
//!   }
//! }
//! ```

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

pub fn config_path(root: &Path) -> std::path::PathBuf {
    crate::config::minic_dir(root).join("mcp.json")
}

pub fn load(root: &Path) -> McpConfig {
    match std::fs::read_to_string(config_path(root)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => McpConfig::default(),
    }
}
