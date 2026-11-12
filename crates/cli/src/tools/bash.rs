//! `bash`: jalankan perintah shell di workspace.
//!
//! - Windows default: `cmd.exe /C <command>` (dapat ditimpa `MINIC_SHELL`).
//! - Sandbox opsional (Docker/bwrap/sandbox-exec); permintaan eksplisit yang
//!   tak tersedia **fail-closed** (tidak dijalankan di host). Lihat `sandbox.rs`.
//! - Cap output diterapkan **saat streaming** (anti-OOM).
//! - Timeout → proses dibunuh (kill-tree) dan hasil ditandai **outcome UNKNOWN**.
//! - Perintah berisiko (klasifikasi `policy/commands.rs`) menyimpan snapshot
//!   whole-tree ke `refs/minic/*` lebih dulu, supaya `--undo` bisa memulihkan
//!   kerusakan yang dilakukan shell (best-effort: butuh repo git).
//!
//! Catatan: tanpa sandbox, perintah shell **tidak** dijail oleh path jail;
//! jalur ini hanya dibatasi permission mode.

use crate::policy::commands::{self, Risk};
use crate::sandbox;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const OUTPUT_CAP_CHARS: usize = 200_000;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

pub struct Bash {
    sandbox_request: String,
    /// Shell dari `.minic/config.toml`. `None` → `MINIC_SHELL` atau default OS.
    shell: Option<String>,
}

impl Bash {
    pub fn new(sandbox_request: impl Into<String>) -> Self {
        Self {
            sandbox_request: sandbox_request.into(),
            shell: None,
        }
    }

    /// Shell eksplisit dari config. Kosong/whitespace diperlakukan sebagai
    /// "tidak diset" supaya nilai config yang tidak berguna tidak memecahkan
    /// `bash` (fallback ke default OS).
    pub fn with_shell(mut self, shell: Option<String>) -> Self {
        self.shell = shell.filter(|s| !s.trim().is_empty());
        self
    }
}

impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the workspace (Windows: cmd.exe /C). Optionally sandboxed. Output is capped; a timeout is reported as an unknown outcome."
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Wall-clock timeout in milliseconds (default 120000)."
                }
            },
            "required": ["command"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AgentError::InvalidRequest("bash: missing 'command'".to_string()))?;
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_TIMEOUT_MS);

            let backend = match sandbox::resolve(&self.sandbox_request) {
                Ok(backend) => backend,
                Err(reason) => {
                    return Ok(ToolResult::err(
                        String::new(),
                        "bash",
                        format!("sandbox unavailable: {reason}"),
                    ));
                }
            };

            let host = host_shell(command, self.shell.as_deref());
            let (program, program_args) =
                sandbox::build_invocation(command, &ctx.cwd, backend, host);

            // Checkpoint whole-tree sebelum perintah berisiko: `bash` tidak
            // di-checkpoint per berkas (`checkpoints::snapshot` hanya dipanggil
            // oleh write_file/edit). Best-effort — tidak pernah menghalangi
            // perintah; bila workspace bukan repo git, tidak ada yang bisa
            // di-undo dan itu dicatat di hasil.
            let checkpoint_note =
                if checkpoint_enabled() && commands::classify(command) != Risk::Safe {
                    match crate::checkpoints::snapshot_tree(&ctx.cwd) {
                        outcome if outcome.is_stored() => Some(outcome.describe()),
                        _ => None,
                    }
                } else {
                    None
                };

            let mut result = run_invocation(
                &program,
                &program_args,
                &ctx.cwd,
                Duration::from_millis(timeout_ms),
            )
            .await?;
            // Beri tahu model & pengguna bahwa ada titik pulih (`--undo`).
            // Tanpa ini, checkpoint tak terlihat dan tidak pernah dipakai.
            if let Some(note) = checkpoint_note {
                result.content.push_str("\n[minic] ");
                result.content.push_str(&note);
            }
            Ok(result)
        })
    }
}

/// Apakah checkpoint whole-tree untuk `bash` aktif? Default aktif; dimatikan
/// eksplisit dengan `MINIC_CHECKPOINT_BASH=off` (mis. workspace non-git yang
/// selalu memicu percobaan git).
fn checkpoint_enabled() -> bool {
    !matches!(
        std::env::var("MINIC_CHECKPOINT_BASH")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "off" | "0" | "false"
    )
}

/// Shell host (dipakai saat tidak ada sandbox).
/// Bangun invokasi shell. Urutan prioritas: `configured` (dari
/// `.minic/config.toml`) → `MINIC_SHELL` → default OS. Shell yang tampak
/// seperti `cmd` memakai `/C`, selain itu `-c`.
fn host_shell(command: &str, configured: Option<&str>) -> (String, Vec<String>) {
    let pick = |shell: &str| {
        let looks_like_cmd = shell.to_ascii_lowercase().contains("cmd");
        let flag = if looks_like_cmd { "/C" } else { "-c" };
        (
            shell.to_string(),
            vec![flag.to_string(), command.to_string()],
        )
    };

    if let Some(shell) = configured
        && !shell.trim().is_empty()
    {
        return pick(shell.trim());
    }
    if let Ok(shell) = std::env::var("MINIC_SHELL")
        && !shell.trim().is_empty()
    {
        return pick(shell.trim());
    }

    let (shell, flag) = if cfg!(windows) {
        ("cmd.exe", "/C")
    } else {
        ("/bin/sh", "-c")
    };
    (
        shell.to_string(),
        vec![flag.to_string(), command.to_string()],
    )
}

async fn run_invocation(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<ToolResult, AgentError> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AgentError::Tool(format!("bash spawn '{program}': {e}")))?;

    let pid = child.id();
    // `kill_on_drop` hanya membunuh proses langsung. Guard ini membunuh
    // seluruh pohon saat future dibatalkan (Ctrl+C di TUI) atau timeout,
    // sehingga cucu proses (mis. `cmd.exe /C` → tool) tidak tertinggal.
    let mut kill_guard = KillTreeGuard(pid);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Tool("bash: no stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::Tool("bash: no stderr pipe".to_string()))?;

    let out_task = tokio::spawn(read_capped(stdout, OUTPUT_CAP_CHARS));
    let err_task = tokio::spawn(read_capped(stderr, OUTPUT_CAP_CHARS));

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            kill_guard.disarm();
            let (stdout, out_trunc) = out_task.await.unwrap_or_default();
            let (stderr, err_trunc) = err_task.await.unwrap_or_default();
            let mut body = merge_streams(&stdout, &stderr);
            if out_trunc || err_trunc {
                body.push_str("\n… [output truncated]");
            }
            let code = status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string());
            body.push_str(&format!("\n[exit code: {code}]"));
            let mut result = ToolResult::ok(String::new(), "bash", body);
            result.is_error = !status.success();
            Ok(result)
        }
        Ok(Err(e)) => {
            kill_guard.disarm();
            out_task.abort();
            err_task.abort();
            Err(AgentError::Tool(format!("bash wait: {e}")))
        }
        Err(_) => {
            // Bunuh pohon lebih dulu agar pipe cucu proses ikut tertutup;
            // kalau tidak, `out_task.await` bisa menggantung.
            if let Some(pid) = pid {
                kill_tree(pid);
            }
            let _ = child.start_kill();
            let (stdout, _) = out_task.await.unwrap_or_default();
            let (stderr, _) = err_task.await.unwrap_or_default();
            let mut body = merge_streams(&stdout, &stderr);
            body.push_str(&format!(
                "\n[timeout after {} ms — outcome UNKNOWN]",
                timeout.as_millis()
            ));
            Ok(ToolResult::err(String::new(), "bash", body))
        }
    }
}

/// Membunuh pohon proses saat di-drop kecuali sudah `disarm()` (proses selesai
/// normal). Dipakai untuk membatalkan tool yang sedang berjalan.
struct KillTreeGuard(Option<u32>);

impl KillTreeGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for KillTreeGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_tree(pid);
        }
    }
}

fn merge_streams(stdout: &str, stderr: &str) -> String {
    let mut body = stdout.to_string();
    if !stderr.is_empty() {
        body.push_str("\n[stderr]\n");
        body.push_str(stderr);
    }
    body.trim_end().to_string()
}

/// Baca reader sampai EOF dengan cap karakter; tetap menguras pipe.
async fn read_capped<R>(mut reader: R, cap: usize) -> (String, bool)
where
    R: AsyncRead + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if buffer.len() >= cap {
                    truncated = true;
                    continue;
                }
                let room = cap - buffer.len();
                let take = n.min(room);
                buffer.extend_from_slice(&chunk[..take]);
                if take < n {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&buffer).into_owned(), truncated)
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod shell_selection_tests {
    use super::*;

    #[test]
    fn configured_shell_wins_over_default() {
        let (shell, argv) = host_shell("echo hi", Some("bash"));
        assert_eq!(shell, "bash");
        assert_eq!(argv, vec!["-c".to_string(), "echo hi".to_string()]);
    }

    #[test]
    fn cmd_like_shell_gets_slash_c() {
        let (shell, argv) = host_shell("dir", Some("C:/Windows/System32/cmd.exe"));
        assert!(shell.to_ascii_lowercase().contains("cmd"));
        assert_eq!(argv[0], "/C");
    }

    #[test]
    fn whitespace_shell_falls_back_to_default() {
        let (shell, _) = host_shell("echo hi", Some("   "));
        assert!(
            shell == "cmd.exe" || shell == "/bin/sh",
            "unexpected default: {shell}"
        );
    }

    #[test]
    fn with_shell_ignores_blank_config() {
        let tool = Bash::new("off").with_shell(Some("  ".to_string()));
        assert!(tool.shell.is_none());
        let tool = Bash::new("off").with_shell(Some("bash".to_string()));
        assert_eq!(tool.shell.as_deref(), Some("bash"));
    }
}
