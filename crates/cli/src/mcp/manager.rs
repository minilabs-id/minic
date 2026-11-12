//! Manajer kumpulan server MCP (koneksi best-effort).

use crate::mcp::client::McpClient;
use crate::mcp::config;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Default)]
pub struct McpManager {
    clients: BTreeMap<String, Arc<McpClient>>,
    errors: Vec<String>,
}

impl McpManager {
    /// Hubungkan semua server di `.minic/mcp.json`. Server yang gagal
    /// dicatat sebagai error, bukan menggagalkan startup.
    pub async fn connect(root: &Path) -> Self {
        let cfg = config::load(root);
        let mut clients = BTreeMap::new();
        let mut errors = Vec::new();
        for (name, spec) in cfg.servers {
            match McpClient::connect(&name, &spec).await {
                Ok(client) => {
                    clients.insert(name, Arc::new(client));
                }
                Err(error) => errors.push(format!("{name}: {error}")),
            }
        }
        Self { clients, errors }
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}
