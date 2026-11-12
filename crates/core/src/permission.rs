//! Kebijakan permission. Kernel hanya tahu *keputusan*; host memutuskan.

use futures_core::future::BoxFuture;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Auto,
    Ask,
    ReadOnly,
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PermissionMode::Auto => "auto",
            PermissionMode::Ask => "ask",
            PermissionMode::ReadOnly => "readonly",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask(String),
}

pub trait PermissionHandler: Send + Sync {
    /// `mutating` menandai apakah tool akan mengubah workspace/efek eksternal;
    /// policy memakainya untuk mode `readonly`.
    fn decide<'a>(
        &'a self,
        tool_name: &'a str,
        args: &'a serde_json::Value,
        mutating: bool,
    ) -> BoxFuture<'a, Decision>;

    /// Selesaikan `Decision::Ask` menjadi Allow/Deny.
    ///
    /// Default **fail-closed**: menolak. Dengan begitu handler yang tidak
    /// menyediakan approver (mis. non-TTY) tidak pernah diam-diam mengizinkan.
    fn resolve_ask<'a>(
        &'a self,
        _tool_name: &'a str,
        _args: &'a serde_json::Value,
        reason: &'a str,
    ) -> BoxFuture<'a, Decision> {
        let reason = reason.to_string();
        Box::pin(async move {
            Decision::Deny(if reason.is_empty() {
                "no approver (fail-closed)".to_string()
            } else {
                format!("no approver (fail-closed): {reason}")
            })
        })
    }
}

/// Handler permisif untuk test/utilitas: selalu mengizinkan.
pub struct AllowAll;

impl PermissionHandler for AllowAll {
    fn decide<'a>(
        &'a self,
        _tool_name: &'a str,
        _args: &'a serde_json::Value,
        _mutating: bool,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async move { Decision::Allow })
    }

    fn resolve_ask<'a>(
        &'a self,
        _tool_name: &'a str,
        _args: &'a serde_json::Value,
        _reason: &'a str,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async move { Decision::Allow })
    }
}
