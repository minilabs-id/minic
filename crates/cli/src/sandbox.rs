//! Sandbox eksekusi shell.
//!
//! Prinsip (mengikuti pelajaran baseline F-03):
//! - Tanpa permintaan sandbox (`off`) → jalankan langsung di host.
//! - `auto` → pakai backend pertama yang tersedia, kalau tidak ada → host.
//! - Backend **eksplisit** (`docker`/`bwrap`/`sandbox-exec`) atau `strict` yang
//!   tidak tersedia → **fail-closed**: perintah TIDAK dijalankan di host.
//!
//! Membuka "dianggap terisolasi padahal tidak" adalah bug keamanan; karena itu
//! tidak ada fallback diam-diam.

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Docker,
    Bwrap,
    SandboxExec,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Docker => "docker",
            Backend::Bwrap => "bwrap",
            Backend::SandboxExec => "sandbox-exec",
        }
    }

    pub fn available(self) -> bool {
        match self {
            Backend::Docker => command_ok("docker", &["--version"]),
            Backend::Bwrap => command_ok("bwrap", &["--version"]),
            Backend::SandboxExec => {
                cfg!(target_os = "macos") && command_ok("sandbox-exec", &["-h"])
            }
        }
    }
}

fn command_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

const ALL: [Backend; 3] = [Backend::Docker, Backend::Bwrap, Backend::SandboxExec];

/// Terjemahkan permintaan sandbox menjadi backend (atau None = host).
pub fn resolve(request: &str) -> Result<Option<Backend>, String> {
    let req = request.trim().to_ascii_lowercase();
    match req.as_str() {
        "" | "0" | "off" | "none" => Ok(None),
        "auto" => Ok(ALL.into_iter().find(|backend| backend.available())),
        "docker" => require(Backend::Docker),
        "bwrap" => require(Backend::Bwrap),
        "sandbox-exec" => require(Backend::SandboxExec),
        "strict" => ALL
            .into_iter()
            .find(|backend| backend.available())
            .map(Some)
            .ok_or_else(|| "MINIC_SANDBOX=strict: no sandbox backend available".to_string()),
        other => Err(format!(
            "unknown sandbox value: '{other}' (use off|auto|docker|bwrap|sandbox-exec|strict)"
        )),
    }
}

fn require(backend: Backend) -> Result<Option<Backend>, String> {
    if backend.available() {
        Ok(Some(backend))
    } else {
        Err(format!(
            "sandbox backend '{}' requested but not available on this host",
            backend.name()
        ))
    }
}

/// Bangun perintah final. `host_shell` dipakai saat tidak ada sandbox.
pub fn build_invocation(
    command: &str,
    cwd: &Path,
    backend: Option<Backend>,
    host_shell: (String, Vec<String>),
) -> (String, Vec<String>) {
    match backend {
        Some(Backend::Docker) => {
            let image =
                std::env::var("MINIC_SANDBOX_IMAGE").unwrap_or_else(|_| "alpine:3".to_string());
            let mount = format!("{}:/workspace", cwd.display());
            let args = vec![
                "run",
                "--rm",
                "--network",
                "none",
                "--read-only",
                "--cap-drop",
                "ALL",
                "--pids-limit",
                "32",
                "--memory",
                "512m",
                "-v",
                mount.as_str(),
                "-w",
                "/workspace",
                image.as_str(),
                "sh",
                "-lc",
                command,
            ];
            (
                "docker".to_string(),
                args.into_iter().map(str::to_string).collect(),
            )
        }
        Some(Backend::Bwrap) => {
            let dir = cwd.display().to_string();
            let args = vec![
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
                "--bind",
                dir.as_str(),
                dir.as_str(),
                "--unshare-net",
                "--die-with-parent",
                "--chdir",
                dir.as_str(),
                "--",
                "/bin/sh",
                "-c",
                command,
            ];
            (
                "bwrap".to_string(),
                args.into_iter().map(str::to_string).collect(),
            )
        }
        Some(Backend::SandboxExec) => {
            let profile = format!(
                "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath \"{}\"))",
                cwd.display()
            );
            let args = vec!["-p", profile.as_str(), "/bin/sh", "-c", command];
            (
                "sandbox-exec".to_string(),
                args.into_iter().map(str::to_string).collect(),
            )
        }
        None => host_shell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_resolves_to_host() {
        assert_eq!(resolve("off").unwrap(), None);
        assert_eq!(resolve("").unwrap(), None);
    }

    #[test]
    fn unknown_value_is_error() {
        assert!(resolve("nonsense").is_err());
    }

    #[test]
    fn docker_wrap_has_hardening_flags() {
        let host = (
            "cmd.exe".to_string(),
            vec!["/C".to_string(), "ls".to_string()],
        );
        let (program, args) =
            build_invocation("ls", Path::new("C:\\ws"), Some(Backend::Docker), host);
        assert_eq!(program, "docker");
        assert!(args.contains(&"--network".to_string()));
        assert!(args.contains(&"none".to_string()));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(args.contains(&"--cap-drop".to_string()));
        assert_eq!(args.last().unwrap(), "ls");
    }

    #[test]
    fn bwrap_wrap_unshares_network() {
        let host = ("sh".to_string(), vec!["-c".to_string(), "ls".to_string()]);
        let (program, args) = build_invocation("ls", Path::new("/ws"), Some(Backend::Bwrap), host);
        assert_eq!(program, "bwrap");
        assert!(args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn sandbox_exec_denies_network() {
        let host = ("sh".to_string(), vec!["-c".to_string(), "ls".to_string()]);
        let (program, args) =
            build_invocation("ls", Path::new("/ws"), Some(Backend::SandboxExec), host);
        assert_eq!(program, "sandbox-exec");
        assert!(args.iter().any(|a| a.contains("deny network*")));
    }

    #[test]
    fn host_passthrough_when_no_backend() {
        let host = (
            "cmd.exe".to_string(),
            vec!["/C".to_string(), "echo hi".to_string()],
        );
        let (program, args) = build_invocation("echo hi", Path::new("."), None, host.clone());
        assert_eq!(program, host.0);
        assert_eq!(args, host.1);
    }
}
