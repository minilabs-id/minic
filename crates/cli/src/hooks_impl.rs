//! Implementasi `AgentHooks` untuk host: pre/post hook dari `.minic/hooks/`.

use minic_core::agent::AgentHooks;
use std::path::PathBuf;

pub struct FileHooks {
    root: PathBuf,
}

impl FileHooks {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl AgentHooks for FileHooks {
    fn pre_tool(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cwd: &std::path::Path,
    ) -> Result<(), String> {
        crate::hooks::run_pre(&self.root, tool, args, cwd)
    }

    fn post_tool(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cwd: &std::path::Path,
        exit_code: i32,
    ) -> Option<String> {
        crate::hooks::run_post(&self.root, tool, args, cwd, exit_code)
    }
}
