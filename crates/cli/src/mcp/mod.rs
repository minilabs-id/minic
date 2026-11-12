//! Klien MCP (Model Context Protocol) — transport stdio JSON-RPC 2.0.

pub mod client;
pub mod config;
pub mod manager;

pub use client::McpClient;
pub use config::{McpConfig, ServerSpec};
pub use manager::McpManager;
