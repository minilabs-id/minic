//! # minic-core
//!
//! Kernel agen yang **murni**: hanya tipe, state machine, dan driver loop.
//! Tidak ada I/O, tidak ada `tokio`, tidak ada jaringan.
//!
//! Prinsip:
//! - Semua I/O diwujudkan lewat trait (`ModelProvider`, `Tool`, `PermissionHandler`)
//!   yang diimplementasikan host. Kernel hanya mengorkestrasi.
//! - Status/keadaan memakai `enum`, bukan string, sehingga `match` dipaksa
//!   lengkap oleh kompilator.
//! - Id memakai newtype agar tak bisa tertukar.

pub mod agent;
pub mod budget;
pub mod compaction;
pub mod error;
pub mod events;
pub mod history;
pub mod permission;
pub mod provider;
pub mod tokens;
pub mod tool;
pub mod types;

pub use error::AgentError;
