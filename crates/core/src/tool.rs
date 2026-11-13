//! Abstraksi tool + registry.

use crate::error::AgentError;
use crate::types::ToolResult;
use futures_core::future::BoxFuture;
use std::path::PathBuf;

/// Konteks yang diberikan ke tool. `cwd` **wajib** datang dari sini, bukan
/// dari `std::env::current_dir()` (pelajaran baseline: cwd tool harus eksplisit).
///
/// Catatan penting: **tidak ada** salinan mode izin di sini. Dulu ada field
/// `permission_mode` yang diisi sekali saat startup dan tidak pernah dibaca
/// tool mana pun; setelah mode bisa diganti saat sesi berjalan (`/discuss`,
/// `/execute`) field itu menjadi nilai basi yang menyesatkan. Aturan izin
/// dibaca dari `PermissionHandler` setiap keputusan — itulah satu-satunya
/// sumber kebenaran.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema untuk parameter tool.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Apakah tool ini bisa mengubah workspace/efek eksternal? Dipakai policy
    /// untuk menolaknya di mode `readonly`. Default: tidak (baca-saja).
    fn is_mutating(&self) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Skema tool dalam bentuk wire `{type:function, function:{...}}`.
    pub fn schemas(&self) -> Vec<serde_json::Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters_schema(),
                    }
                })
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
