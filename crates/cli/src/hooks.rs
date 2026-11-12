//! Hooks: skrip eksternal yang dijalankan sebelum/sesudah tool.
//!
//! Konvensi: skrip di `.minic/hooks/pre_<tool>.<ext>` dan
//! `.minic/hooks/post_<tool>.<ext>` (`.ps1`, `.cmd`, `.bat`, `.sh`).
//! Skrip membaca payload JSON dari variabel lingkungan `MINIC_HOOK_PAYLOAD`
//! (lebih aman dari argv/stdin terhadap quoting `cmd.exe`):
//! `{"tool":"…","args":{…},"cwd":"…"}` (post juga `{"exit_code":…}`).
//!
//! Semantik: pre-hook yang gagal → tool **tidak** dijalankan; post-hook yang
//! gagal hanya dicatat ke stderr (tidak menggagalkan turn). Tanpa skrip,
//! hook adalah no-op sehingga instalasi minimal tidak terpengaruh.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn hook_path(root: &Path, phase: &str, tool: &str) -> Option<PathBuf> {
    let dir = root.join(".minic").join("hooks");
    let names = [
        format!("{phase}_{tool}.ps1"),
        format!("{phase}_{tool}.cmd"),
        format!("{phase}_{tool}.bat"),
        format!("{phase}_{tool}.sh"),
    ];
    names
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}

fn is_extension(script: &Path, extension: &str) -> bool {
    script
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// Lepas prefiks verbatim `\\?\` yang ditambahkan `fs::canonicalize` di
/// Windows — `cmd.exe`/PowerShell tidak bisa menjalankan path seperti itu
/// (pelajaran yang sama sudah dikodekan di `policy/jail.rs`).
fn command_path(script: &Path) -> PathBuf {
    let lossy = script.to_string_lossy();
    let cleaned = lossy
        .strip_prefix(r"\\?\")
        .map(|rest| rest.to_string())
        .unwrap_or_else(|| lossy.to_string());
    PathBuf::from(cleaned)
}

fn command_for(script: &Path) -> Command {
    let script = command_path(script);
    if is_extension(&script, "ps1") {
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script);
        cmd
    } else if cfg!(windows) {
        // `.cmd`/`.bat` butuh cmd.exe; `.sh` di Windows juga lewat cmd
        // (best-effort; jangan pura-pura bisa menjalankan skrip POSIX).
        //
        // Tidak perlu manual-quote: `std::process::Command` sudah menerapkan
        // aturan quoting MSVCRT pada command line Windows, dan `cmd.exe /C`
        // menerima path ber-spasi yang di-quote dengan benar.
        let mut cmd = Command::new("cmd.exe");
        cmd.arg("/C").arg(&script);
        cmd
    } else {
        Command::new(script)
    }
}

fn run_script(
    script: &Path,
    payload: &serde_json::Value,
    cwd: &Path,
) -> std::io::Result<std::process::Output> {
    command_for(script)
        .current_dir(command_path(cwd))
        .env("MINIC_HOOK_PAYLOAD", payload.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
}

/// `Ok(())` = lanjut; `Err` = tolak (pre-hook gagal / tidak bisa jalan).
pub fn run_pre(
    root: &Path,
    tool: &str,
    args: &serde_json::Value,
    cwd: &Path,
) -> Result<(), String> {
    let Some(script) = hook_path(root, "pre", tool) else {
        return Ok(());
    };
    let payload = serde_json::json!({ "tool": tool, "args": args, "cwd": cwd.to_string_lossy() });
    let output = run_script(&script, &payload, cwd).map_err(|e| format!("pre-hook spawn: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    let status = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    Err(format!(
        "pre-hook for {tool} failed (exit {status}): {detail}"
    ))
}

/// Jalankan post-hook. Mengembalikan pesan peringatan bila skrip gagal, supaya
/// host bisa menampilkannya di transcript (stderr tidak terlihat di TUI).
pub fn run_post(
    root: &Path,
    tool: &str,
    args: &serde_json::Value,
    cwd: &Path,
    exit_code: i32,
) -> Option<String> {
    let script = hook_path(root, "post", tool)?;
    let payload = serde_json::json!({
        "tool": tool,
        "args": args,
        "cwd": cwd.to_string_lossy(),
        "exit_code": exit_code
    });
    match run_script(&script, &payload, cwd) {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "{tool} post-hook exited with {:?}",
            output.status.code()
        )),
        Err(error) => Some(format!("{tool} post-hook failed: {error}")),
    }
}
