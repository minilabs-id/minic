//! Binary `minic` — CLI tipis di atas kernel.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use minic::{
    auth, checkpoints, config, plans, policy, provider, render, store, telemetry, tools, tui,
};
use minic_core::agent::{HistoryPersistence, RunOptions, run_agent};
use minic_core::events::AgentEvent;
use minic_core::history::ContextStore;
use minic_core::permission::PermissionMode;
use minic_core::provider::ModelProvider;
use minic_core::tool::{ToolContext, ToolRegistry};
use minic_core::types::Message;

const DEFAULT_SYSTEM: &str = r"You are minic, a careful terminal coding agent working in the user's workspace.

Ground rules:
- Inspect before you change: read a file (or grep for the definition) before editing it.
- Make the smallest change that solves the problem; do not rewrite whole files to fix one line.
- After changing code, verify it. Run the project's test/check command (e.g. `cargo test`,
  `npm test`, `pytest`) or the command given by --verify. If it fails, fix the cause before
  reporting success. Never claim a change works without evidence from a command you ran.
- If a tool call fails or its arguments are malformed, read the error and correct it; do not
  repeat the same call unchanged.
- Report what you actually checked, and say plainly what you did not.";

#[derive(Parser, Debug)]
#[command(name = "minic", version, about = "Native Rust terminal coding agent")]
struct Cli {
    /// Prompt / task. Jika kosong, dibaca dari stdin. (Diabaikan dengan `--tui`.)
    prompt: Option<String>,

    /// Model id (wajib kecuali `--fake`; tidak ada default).
    #[arg(long)]
    model: Option<String>,

    /// Base URL OpenAI-compatible, mis. `https://api.openai.com/v1`.
    #[arg(long)]
    base_url: Option<String>,

    /// API key (else MINIC_API_KEY / OPENAI_API_KEY / .minic/auth.json / config).
    #[arg(long)]
    api_key: Option<String>,

    /// System prompt.
    #[arg(long)]
    system: Option<String>,

    /// Permission mode.
    #[arg(long, value_enum, default_value = "auto")]
    mode: ModeArg,

    /// Lanjutkan sesi ini (dibuat bila belum ada).
    #[arg(long)]
    session: Option<String>,

    /// Lanjutkan sesi terakhir di workspace ini.
    #[arg(long)]
    resume: bool,

    /// Tampilkan daftar sesi lalu keluar.
    #[arg(long)]
    list_sessions: bool,

    /// Pulihkan berkas dari checkpoint terakhir lalu keluar.
    #[arg(long)]
    undo: bool,

    /// Tampilkan daftar checkpoint lalu keluar.
    #[arg(long)]
    list_checkpoints: bool,

    /// Simpan API key yang terdeteksi ke `.minic/auth.json` lalu keluar.
    #[arg(long)]
    save_key: bool,

    /// Buka TUI interaktif (butuh TTY).
    #[arg(long)]
    tui: bool,

    /// Sandbox untuk `bash`: off|auto|docker|bwrap|sandbox-exec|strict.
    #[arg(long)]
    sandbox: Option<String>,

    /// Pancarkan event JSONL ke stdout (mesin/conformance).
    #[arg(long)]
    json: bool,

    /// Tampilkan reasoning model (jika disediakan provider) di render linear.
    #[arg(long)]
    verbose: bool,

    /// Batas langkah agent loop.
    #[arg(long, default_value_t = 50)]
    max_steps: u32,

    /// Workspace root (dipakai sebagai cwd tool + path jail).
    #[arg(long, default_value = ".")]
    cwd: String,

    /// Pakai provider fake deterministik (tanpa jaringan).
    #[arg(long)]
    fake: bool,

    /// Daftar rencana tersimpan di `.minic/plans/` lalu keluar (tanpa model).
    #[arg(long)]
    plans: bool,

    /// Tampilkan satu rencana tersimpan lalu keluar (tanpa model).
    #[arg(long, value_name = "ID")]
    plan_show: Option<String>,

    /// Batas token output per respons model. Tanpa ini provider memakai
    /// default-nya, dan respons panjang bisa terpotong di tengah tool call.
    #[arg(long, value_name = "N")]
    max_output_tokens: Option<u32>,

    /// Perintah verifikasi yang dijalankan lewat `bash` setelah tool yang
    /// mengubah berkas berhasil (mis. "cargo check"). Hasilnya diberikan ke
    /// model agar ia memperbaiki sendiri, bukan menunggu manusia.
    #[arg(long, value_name = "CMD")]
    verify: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ModeArg {
    Auto,
    Ask,
    Readonly,
}

impl From<ModeArg> for PermissionMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Auto => PermissionMode::Auto,
            ModeArg::Ask => PermissionMode::Ask,
            ModeArg::Readonly => PermissionMode::ReadOnly,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let root = std::fs::canonicalize(&cli.cwd).unwrap_or_else(|_| PathBuf::from(&cli.cwd));
    let display_root = display_path(&root);
    config::ensure_gitignore(&root);
    let cfg = config::FileConfig::load(&root);
    let telemetry = telemetry::Telemetry::from_env(&root);
    let mode: PermissionMode = cli.mode.into();

    // Inspeksi rencana: murni baca berkas, tidak menyentuh model/network.
    if cli.plans {
        print_plan_list(&root);
        return Ok(());
    }
    if let Some(id) = &cli.plan_show {
        print_one_plan(&root, id)?;
        return Ok(());
    }

    if cli.save_key {
        let key = resolve_api_key(&cli, &root, cfg.api_key.clone());
        if key.is_empty() {
            bail!("--save-key: no API key found to save");
        }
        let path = auth::save(&root, &key)?;
        println!("saved API key to {}", path.display());
        return Ok(());
    }

    let store = store::SessionStore::open(&root)?;

    if cli.list_sessions {
        for info in store.list_sessions()? {
            println!(
                "{}\t{} messages\tupdated {}",
                info.id, info.messages, info.updated_at
            );
        }
        return Ok(());
    }

    if cli.list_checkpoints {
        let entries = checkpoints::list(&root);
        if entries.is_empty() {
            println!("no checkpoints");
        }
        for entry in entries {
            println!("{entry}");
        }
        return Ok(());
    }

    if cli.undo {
        match checkpoints::undo_last(&root) {
            Ok(message) => println!("{message}"),
            Err(reason) => bail!("undo failed: {reason}"),
        }
        return Ok(());
    }

    // TUI hanya mungkin dengan TTY. Diputuskan lebih awal karena policy perlu
    // tahu jalur approval (modal channel vs prompt terminal).
    let tui_active = cli.tui && minic::approval::is_interactive();
    if cli.tui && !tui_active {
        eprintln!("warning: --tui ignored (no TTY); running non-interactive");
    }

    let model = cli
        .model
        .clone()
        .or(cfg.model)
        .or_else(|| std::env::var("MINIC_MODEL").ok())
        .unwrap_or_default();
    // TUI boleh dibuka tanpa model: pengguna menghubungkan provider lewat
    // `/connect` lalu memilih model lewat `/model`. Mode non-TUI tetap wajib
    // punya model — tidak ada default diam-diam.
    if !cli.fake && !tui_active && model.trim().is_empty() {
        bail!(
            "missing model: pass --model or set MINIC_MODEL / .minic/config.toml (model has no default)"
        );
    }
    // Provider fake tidak butuh model nyata; beri label eksplisit agar status
    // TUI tidak kosong dan kontrak "model wajib" tetap berlaku untuk provider asli.
    let model = if cli.fake && model.trim().is_empty() {
        "fake".to_string()
    } else {
        model
    };
    let system = cli
        .system
        .clone()
        .or(cfg.system)
        .or_else(|| Some(DEFAULT_SYSTEM.to_string()));

    let base_url = resolve_base_url(&cli, cfg.base_url);
    let api_key = resolve_api_key(&cli, &root, cfg.api_key);
    if !cli.fake && !tui_active && api_key.trim().is_empty() {
        bail!("missing API key: pass --api-key or set MINIC_API_KEY / OPENAI_API_KEY");
    }

    let mut provider: Option<Arc<dyn ModelProvider>> = None;
    let mut remote: Option<Arc<provider::openai_compat::OpenAiCompat>> = None;
    let mut fetched_window: Option<u32> = None;
    // Env `MINIC_CONTEXT_TOKENS` selalu menang; tidak perlu fetch bila sudah
    // ditentukan pengguna (hemat satu round-trip jaringan saat startup).
    let context_override: Option<u32> = std::env::var("MINIC_CONTEXT_TOKENS")
        .ok()
        .and_then(|value| value.parse().ok());
    if cli.fake {
        provider = Some(Arc::new(provider::fake::FakeProvider::default()));
    } else if !api_key.trim().is_empty() {
        let openai = Arc::new(provider::openai_compat::OpenAiCompat::new(
            base_url.clone(),
            api_key.clone(),
        ));
        // TUI boleh masuk tanpa model; context window dideteksi saat model
        // dipilih. Sesi non-TUI sudah pasti punya model di titik ini.
        if context_override.is_none() && !model.trim().is_empty() {
            fetched_window = openai.fetch_context_window(&model).await;
        }
        remote = Some(openai.clone());
        provider = Some(openai);
    }

    // Context window model: `MINIC_CONTEXT_TOKENS` menimpa; kalau tidak,
    // pakai `context_length` dari `/v1/models` (best-effort). Model besar
    // (DeepSeek 1M, dll.) punya window jauh lebih besar dari default 128k —
    // ini mencegah kompaksi prematur.
    let context_tokens: u32 = context_override.or(fetched_window).unwrap_or(128_000);
    // Satu sesi model dipakai TUI, sub-agent, dan turn: mengganti provider/
    // model saat runtime tidak perlu restart atau rebuild registry.
    let model_session = Arc::new(minic::model_session::ModelSession::new(
        provider.clone(),
        model.clone(),
        context_tokens,
    ));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(tools::read_file::ReadFile));
    registry.register(Box::new(tools::write_file::WriteFile));
    registry.register(Box::new(tools::edit::Edit));
    let sandbox_request = cli
        .sandbox
        .clone()
        .or(cfg.sandbox.clone())
        .or_else(|| std::env::var("MINIC_SANDBOX").ok())
        .unwrap_or_else(|| "off".to_string());
    match minic::sandbox::resolve(&sandbox_request) {
        Err(reason) => eprintln!("warning: {reason}; bash will refuse to run commands"),
        Ok(None) => {
            let normalized = sandbox_request.trim().to_ascii_lowercase();
            if !matches!(normalized.as_str(), "" | "0" | "off" | "none") {
                eprintln!(
                    "warning: sandbox '{sandbox_request}' has no available backend; bash will run on the host"
                );
            }
        }
        Ok(Some(_)) => {}
    }
    registry.register(Box::new(
        tools::bash::Bash::new(sandbox_request.clone()).with_shell(cfg.shell.clone()),
    ));
    registry.register(Box::new(tools::grep::Grep));
    registry.register(Box::new(tools::glob::Glob));
    // Tool LSP diregistrasi hanya bila ada konfigurasi server — tool mati
    // (tanpa server) hanya boros token & membingungkan model.
    if !minic::lsp::load_config(&root).servers.is_empty() {
        registry.register(Box::new(tools::lsp::LspDiagnostics));
    }

    // Sub-agent pool (tanpa nesting). Pool membaca sesi model yang sama, jadi
    // pergantian model di TUI ikut terpakai. `max_subagents` hanya boleh
    // menurunkan batas, tidak menaikkannya.
    let subagent_pool = Arc::new(minic::subagent::SubAgentPool::with_capacity(
        model_session.clone(),
        system.clone(),
        cli.max_steps.min(30),
        root.clone(),
        cfg.max_subagents.unwrap_or(minic::subagent::MAX_SUBAGENTS),
    ));
    registry.register(Box::new(tools::delegate::DelegateTask::new(subagent_pool)));

    // MCP (best-effort): server yang gagal dicatat, bukan menggagalkan startup.
    let mcp_manager = Arc::new(minic::mcp::McpManager::connect(&root).await);
    for error in mcp_manager.errors() {
        eprintln!("warning: mcp {error}");
    }
    if !mcp_manager.is_empty() {
        registry.register(Box::new(tools::mcp::McpCall::new(mcp_manager.clone())));
        registry.register(Box::new(tools::mcp::McpList::new(mcp_manager.clone())));
    }

    let (approval_tx, approval_rx) = if tui_active {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let policy = policy::SimplePolicy::new(root.clone(), mode);
    // Guard perintah `bash`: klasifikasi perintah berisiko selalu aktif kecuali
    // pengguna mematikannya secara eksplisit (`MINIC_BASH_GUARD=off`).
    let shell_guard = !matches!(
        std::env::var("MINIC_BASH_GUARD")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "off" | "0" | "false"
    );
    let policy = policy
        .with_shell_guard(shell_guard)
        .with_extra_deny(cfg.bash_deny_extra.clone());
    let policy = match approval_tx {
        Some(tx) => policy.with_ask_route(policy::AskRoute::Tui {
            tx,
            always: std::sync::Mutex::new(std::collections::HashSet::new()),
        }),
        None => policy,
    };
    let tool_ctx = ToolContext {
        cwd: root.clone(),
        permission_mode: mode,
    };
    let hooks = minic::hooks_impl::FileHooks::new(root.clone());

    let session_id = match cli.session.clone() {
        Some(id) => id,
        None if cli.resume => store.latest_session()?.unwrap_or_else(new_session_id),
        None => new_session_id(),
    };

    let restoring = cli.resume || cli.session.is_some();
    let mut history = if restoring {
        ContextStore::from_messages(store.load_messages(&session_id)?)
    } else {
        ContextStore::new()
    };
    store.upsert_session(&session_id, &model, &display_root)?;
    eprintln!("session {session_id}");

    // Satu adapter persistensi dipakai bersama TUI & linear: kernel menyimpan
    // snapshot di setiap akhir turn, jadi Ctrl+C / error provider tidak
    // menghapus kerja yang sudah selesai.
    let persistence = Arc::new(store::SessionPersistence::new(store, session_id.clone()));

    // Mode interaktif: TUI multi-turn.
    if tui_active {
        let deps = tui::TuiDeps {
            session: model_session.clone(),
            remote,
            base_url,
            api_key,
            registry: Arc::new(registry),
            policy: Arc::new(policy),
            hooks: Arc::new(hooks),
            tool_ctx,
            system: system.clone(),
            max_steps: cli.max_steps,
            verify_command: resolve_verify_command(&cli),
            mode: mode.to_string(),
            cwd: display_root.clone(),
            persistence: persistence.clone(),
            approvals: approval_rx.expect("approval receiver for TUI"),
            verbose: cli.verbose,
        };
        return tui::run(deps, history.messages().to_vec()).await;
    }

    // Mode satu-shot (linear / JSON).
    let provider = provider.expect("provider available in non-TUI mode");
    let prompt = match cli.prompt.clone() {
        Some(p) if !p.trim().is_empty() => p,
        _ => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim().to_string()
        }
    };
    if prompt.is_empty() {
        bail!("no prompt provided (pass a positional argument, pipe via stdin, or use --tui)");
    }
    history.push(Message::user(prompt));

    let json_mode = cli.json;
    if json_mode {
        println!("{}", render::json::session_line(&session_id, &model));
    }

    let options = RunOptions {
        model,
        system,
        max_steps: cli.max_steps,
        max_tokens: resolve_max_output_tokens(&cli),
        context_window_tokens: Some(context_tokens),
        keep_recent: minic_core::compaction::DEFAULT_KEEP_RECENT,
        verify_command: resolve_verify_command(&cli),
    };

    let mut on_event = |event: AgentEvent| {
        if json_mode {
            render::json::write(&event);
        } else {
            render::handle(&event, cli.verbose);
        }
        telemetry.record(&event);
    };
    // Simpan state awal (prompt user) — kalau provider gagal di turn pertama,
    // prompt tidak hilang; sesi bisa di-resume.
    persistence.save(history.messages());
    let outcome = match run_agent(
        provider.as_ref(),
        &registry,
        &policy,
        &hooks,
        &mut history,
        &tool_ctx,
        &options,
        Some(persistence.as_ref()),
        &mut on_event,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(minic_core::error::AgentError::MaxSteps(message)) => {
            eprintln!("error: {message} (max steps reached)");
            std::process::exit(2);
        }
        Err(minic_core::error::AgentError::Aborted) => {
            eprintln!("error: aborted by user");
            std::process::exit(130);
        }
        Err(error) => return Err(error.into()),
    };

    // Snapshot akhir (streaming persistensi sudah menyimpan per-turn; ini
    // menutup pesan yang mungkin terlewati & update `updated_at`).
    let written = {
        let mut store = persistence.store().lock().expect("store lock");
        store.save_messages(&session_id, history.messages())?
    };
    if !json_mode {
        println!();
        render::final_summary(&outcome);
    }
    tracing::debug!(
        steps = outcome.steps,
        tool_calls = outcome.tool_calls,
        persisted = written,
        "agent finished"
    );
    Ok(())
}

fn new_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("s-{millis}")
}

/// Path untuk ditampilkan ke pengguna: buang prefix verbatim Windows (`\\?\`)
/// yang ditambahkan `canonicalize` dan hanya menambah kebisingan di UI.
fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
}

/// `--plans`: daftar rencana tersimpan. Deterministik, tanpa model.
fn print_plan_list(root: &std::path::Path) {
    let plans = plans::list(root);
    if plans.is_empty() {
        println!(
            "no saved plans in {}",
            display_path(&plans::plans_dir(root))
        );
        return;
    }
    for (plan, warning) in plans {
        if let Some(warning) = warning {
            println!("{}  ({} task(s)) ·{}", plan.id, plan.tasks.len(), warning);
            continue;
        }
        println!(
            "{}  ({}) · {} task(s)",
            plan.id,
            plan.status.as_str(),
            plan.tasks.len()
        );
    }
}

/// `--plan-show <id>`: isi satu rencana. Id tidak valid / tidak ada → error
/// eksplisit (bukan keluaran kosong yang membingungkan).
fn print_one_plan(root: &std::path::Path, id: &str) -> Result<()> {
    let plan = match plans::load(root, id).map_err(anyhow::Error::msg)? {
        Some(plan) => plan,
        None => bail!(
            "no saved plan named {id:?} in {}",
            display_path(&plans::plans_dir(root))
        ),
    };
    println!("plan {}  ({})", plan.id, plan.status.as_str());
    println!("created_ms  {}", plan.created_ms);
    if let Some(executed) = plan.executed_ms {
        println!("executed_ms {executed}");
    }
    println!("tasks       {}", plan.tasks.len());
    for (index, task) in plan.tasks.iter().enumerate() {
        println!("  {}. {task}", index + 1);
    }
    if plan.status == plans::PlanStatus::Draft {
        println!("\nrun it with: delegate_task plan_id={} run=true", plan.id);
    }
    Ok(())
}

/// Batas output: `--max-output-tokens` → `MINIC_MAX_OUTPUT_TOKENS` → None.
/// Nilai 0 atau tidak valid diabaikan (lebih baik memakai default provider
/// daripada mengirim `max_tokens: 0` yang membuat model tidak bisa menjawab).
fn resolve_max_output_tokens(cli: &Cli) -> Option<u32> {
    cli.max_output_tokens.filter(|n| *n > 0).or_else(|| {
        std::env::var("MINIC_MAX_OUTPUT_TOKENS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
    })
}

/// Perintah verifikasi: `--verify` → `MINIC_VERIFY` → None.
/// String kosong/whitespace diabaikan (menjalankan `bash` dengan perintah
/// kosong hanya menghasilkan kebisingan).
fn resolve_verify_command(cli: &Cli) -> Option<String> {
    cli.verify
        .clone()
        .or_else(|| std::env::var("MINIC_VERIFY").ok())
        .map(|cmd| cmd.trim().to_string())
        .filter(|cmd| !cmd.is_empty())
}

fn resolve_base_url(cli: &Cli, from_file: Option<String>) -> String {
    cli.base_url
        .clone()
        .or(from_file)
        .or_else(|| std::env::var("MINIC_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
}

fn resolve_api_key(cli: &Cli, root: &std::path::Path, from_file: Option<String>) -> String {
    cli.api_key
        .clone()
        .or_else(|| std::env::var("MINIC_API_KEY").ok())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .or_else(|| auth::load(root).api_key)
        .or(from_file)
        .unwrap_or_default()
}
