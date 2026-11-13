# minic

**v0.3.1** — Native Rust terminal coding agent.

```powershell
winget install ParticleAI.minic
minic
```

This repository implements milestones **M0–M6** plus a reliability upscale
(streaming session persistence, provider retry/backoff, auto context-window
detection, conditional tool registration, meaningful exit codes) and an
autonomy/transparency hardening pass (audited permission decisions, a
risky-command shell guard with checkpoints, category-scoped approvals, a
read-only tool-output panel, and reviewable saved plans).

v0.2.0–v0.3.1 added response integrity (truncated and malformed tool calls are
never executed), a verification loop, `.gitignore`-aware search, parallel
read-only tool calls, a floating command autocomplete, and **work modes** —
`/discuss` (read-only, enforced in the permission layer) and `/execute`.

See [`CHANGELOG.md`](./CHANGELOG.md) for what changed, and
[`PLAN.md`](./PLAN.md) for the full plan, decisions and roadmap.

## Status

- **M0**: provider streaming (OpenAI-compatible) + agent loop + linear stdout.
- **M1**: tools `read_file`, `write_file`, `edit`, `bash`, `grep`, `glob`;
  workspace path jail + sensitive-file denial; `--mode auto|ask|readonly`;
  opt-in local telemetry.
- **M2**: interactive approval for `--mode ask` (TTY prompt in linear mode,
  in-TUI modal when running `--tui`), **fail-closed in non-TTY / closed
  channel**; permission matrix tested.
- **M3**: durable session persistence (SQLite) with prefix-compare append,
  `--session` / `--resume` / `--list-sessions`, and local API key storage
  (`.minic/auth.json`).
- **M4**: `--json` machine-readable event stream (conformance surface) and
  property tests (SSE split-invariance, edit matcher).
- **M5**: interactive TUI (ratatui/crossterm) with multi-turn REPL, streaming
  transcript, one-line tool ledger, and dim status line. State machine
  unit-tested and real PTY test (M6-f).
- **M6-a**: optional shell sandbox (Docker / bwrap / sandbox-exec) with
  **fail-closed** when a requested backend is unavailable.
- **M6-b**: file checkpoints + `--undo` / `--list-checkpoints`; when the
  workspace is a git repo, whole-tree snapshots are also stored under
  `refs/minic/*` (`git stash create`, index/HEAD untouched).
- **M6-c**: MCP client (stdio JSON-RPC) exposing `mcp_list` / `mcp_call` from
  `.minic/mcp.json`.
- **M6-d**: `delegate_task` — up to **10** parallel read-only sub-agents with
  separate context, no nesting.
- **M6-e**: mechanical context compaction when history nears the window
  (lossy; keeps recent messages and valid tool pairing).
- **M6-f**: real PTY test for the TUI (alt-screen enter + restore on Ctrl+C).
- **M6-g**: LSP client (`lsp_diagnostics`) driven by `.minic/lsp.json`
  (per-extension server), with a fake LSP server binary for tests.
- **M6-h**: pre/post tool hooks from `.minic/hooks/{pre,post}_<tool>.{ps1,cmd,bat,sh}`
  (payload in `MINIC_HOOK_PAYLOAD`); pre-hook failure blocks the tool.
- **M6-i**: precise tokenizer (BPE `cl100k_base` via `tiktoken-rs`, feature
  `tiktoken`) for compaction budgeting, with a chars/4 heuristic fallback.
- **M6-j (reliability upscale)**:
  - history is persisted at **every turn boundary** (not only at the end), and
    the initial prompt is saved before the first model call — a provider error
    or process death no longer leaves an empty session;
  - provider requests retry transient failures (429 / 500–504) with exponential
    backoff, honoring `Retry-After`; other 4xx fail fast;
  - the context window is read from `/v1/models` (`context_length`) when
    `MINIC_CONTEXT_TOKENS` is unset, avoiding premature compaction;
  - `lsp_diagnostics` is only registered when `.minic/lsp.json` defines at
    least one server;
  - `--verbose` streams model reasoning; exit codes distinguish success (0),
    provider/transport errors (1), max steps (2), and user abort (130);
  - in the TUI, Ctrl+C during streaming cancels the in-flight turn (task
    aborted, session stays open); Ctrl+C while idle exits as before.
- **UI/UX polish (TUI redesign)**:
  - cleaner layout: bottom-anchored transcript, no borders/ornaments outside
    the approval modal, one dim status line, and a multi-line `❯` composer;
  - tool calls are transparent but compact: one line per call
    (`✓ read_file  src/main.rs  12ms`) updated in place, error details on the
    following line;
  - full keyboard editing: prompt history (Up/Down), transcript scroll
    (PgUp/PgDn), newline (Shift+Enter / Ctrl+J), bracketed paste, and
    Ctrl+U/K/W line editing;
  - approval in `--mode ask` is an in-TUI modal (`y` once / `a` always for that
    tool this session / `n` deny); a closed approval channel fails closed;
  - all model/tool text is sanitized before display (ANSI/control sequences),
    and widths are measured in terminal columns (CJK/emoji safe);
  - resumed sessions render their previous transcript instead of an empty
    screen; canceled turns cannot leak late events into the UI.
- **Autonomy & transparency hardening**:
  - every permission decision is audited (`tool_decided` in `--json`/trace,
    `⊘ … denied` in the transcript); a pre-hook denial is now visible too;
  - risky `bash` commands (destructive, publishing, remote-code, privilege,
    host-control) are **promoted to approval** even in `--mode auto`, and take
    a whole-tree checkpoint first so `--undo` can recover;
  - approval is scoped per tool *or* per risk category (`[y]`/`[a]`/`[c]`),
    with a scrollable full-argument panel (`?`);
  - a tool's full output is readable in a read-only panel (`Enter` on an empty
    composer) instead of being hidden behind a one-line summary;
  - `read_file`/`grep` accept `offset`/`limit` for paging large files;
  - `delegate_task` can save a **reviewable plan** before spending tokens.
- Windows-first (host: `x86_64-pc-windows-msvc`).

## Layout

```
crates/core   # minic-core: pure, I/O-free kernel (no tokio, no network)
crates/cli    # minic: providers, tools, policy, render, binary
```

Boundary rule: `core` must not depend on `tokio`, `reqwest`, or any I/O.
Async/runtime lives only at the edges.

## Build & run

```bash
cargo build
cargo run -- --fake "inspect the project"      # no network, deterministic
```

Real provider (OpenAI-compatible):

```bash
cargo run -- --base-url https://api.openai.com/v1 --model gpt-4o-mini "explain this repo"
```

Model resolution order: `--model` → `MINIC_MODEL` → `.minic/config.toml`.
There is **no hardcoded model default**; a model is required unless `--fake`
or `--tui`. The TUI can open without a provider/model and lets you connect
later with `/connect` and `/model` (see below).

API key resolution order: `--api-key` → `MINIC_API_KEY` → `OPENAI_API_KEY` →
`.minic/config.toml` in the workspace.

Base URL resolution order: `--base-url` → `MINIC_BASE_URL` →
`.minic/config.toml` → `https://api.openai.com/v1`.

Permission modes:

```bash
cargo run -- --mode auto     --model <m> "fix the failing test"  # allow inside the jail
cargo run -- --mode ask      --model <m> "refactor this"         # prompt before mutating
cargo run -- --mode readonly --model <m> "review this change"    # deny mutating tools
```

In `ask`, a non-interactive session (pipe/CI) **denies** every mutating action
(fail-closed); there is no way to silently approve.

### Shell guard (`bash`)

`bash` is the one tool that is **not** covered by the workspace jail: whatever
the command does (network, `git push`, delete outside the workspace) happens.
So minic classifies every `bash` command and **promotes risky ones to the
approval path even in `--mode auto`** (see `policy/commands.rs`):

| Category | Examples |
|---|---|
| `destructive` | `rm -rf`, `rmdir /s`, `del /f /s`, `git reset --hard`, `git clean -fdx`, `format`, `dd of=/dev/…`, `shred`, `truncate -s 0` |
| `publishes` | `git push` (unless `--dry-run`/`-n`), `git tag <v>`, `gh release`, `npm/cargo/pip publish`, `docker push` |
| `remote-code` | `curl … \| sh`, `wget … \| bash`, `iwr … \| iex` |
| `privilege` | `sudo`, `runas`, `Start-Process -Verb RunAs`, `chmod -R 777`, `takeown/icacls /grant` |
| `host-control` | `shutdown`, `reboot`, `Stop-Computer`, `taskkill` (anything but minic's own kill-tree), `systemctl stop/disable/mask` |

Approval is not a block: the command still runs once you approve it. In a
non-TTY session the promotion is **fail-closed** — a risky command is denied
instead of silently executed. Ordinary builds/tests (`cargo test`, `git status`,
`npm run build`) are unaffected.

Extensions:

- `.minic/config.toml`: `bash_deny_extra = ["terraform apply", "kubectl delete"]`
  — extra substrings that always need approval.
- `MINIC_BASH_GUARD=off` — disable the classifier entirely (not recommended;
  the previous behaviour returns, where `auto` runs anything).
- The classification is **best-effort text matching**, not a shell parser: it can
  be evaded with obfuscation (`git${IFS}push`, generated scripts). Use
  `--sandbox` for real isolation.

## Context compaction

When the estimated history exceeds 80% of the context window, older messages
are folded into a single summary and the most recent 8 are kept. The window is
`MINIC_CONTEXT_TOKENS` when set; otherwise minic asks the provider's
`/v1/models` endpoint for the model's `context_length`, falling back to 128000.
The env override also wins when you change models from the TUI picker.
Compaction is **lossy** and mechanical (no extra model call); the kept window
never starts with an orphan tool result. Durable state belongs in tasks/files,
not chat history.

Token estimation uses a real BPE tokenizer (`cl100k_base`, tiktoken-rs) in the
`minic` binary; the kernel falls back to a chars/4 heuristic when the
`tiktoken` feature is disabled (it is off by default in `minic-core` itself).

## Sub-agents

`delegate_task` runs up to **10** independent read-only investigation tasks in
parallel, each with its own context, and returns their reports:

```json
{ "tasks": ["find auth entrypoints", "map the test layout"] }
```

Sub-agents only get `read_file` / `grep` / `glob`; they cannot modify files or
delegate further (no nesting). The pool caps concurrency at 10; more than 10
tasks in one call is rejected. The tool is treated as mutating and is gated by
`--mode`.

Lower the cap with `max_subagents` in `.minic/config.toml` (see
[Config](#config-project-local)); values above 10 are clamped down, never up.

### Saved plans

Inline `tasks` run immediately, which means an expensive delegation happens
before you can look at it. Passing a `plan_id` instead **saves** the task list
and runs nothing:

```json
{ "plan_id": "audit-auth", "tasks": ["find auth entrypoints", "map the test layout"] }
```

That writes `.minic/plans/audit-auth.json` and returns
`plan saved (not executed)`. Review it without a model:

```bash
minic --plans                    # list saved plans and their status
minic --plan-show audit-auth     # show one plan
```

Then execute it deliberately:

```json
{ "plan_id": "audit-auth", "run": true }
```

A plan that never succeeded is left as `draft` (so it can be retried); once at
least one sub-agent returns a report the plan is marked `executed` along with
the timestamp. `run` without `plan_id` is rejected, and the sub-agent limit
applies to saved plans too.

Plan ids are restricted to letters, digits, `-` and `_` (max 64 characters).
That is a security boundary, not cosmetics: the id becomes a filename, so
`"plan_id": "../config"` would otherwise write outside `.minic/plans/`. Plans
are deliberately **not** gitignored — they are meant to be committed and
diffed. A corrupt plan file is reported as an error rather than silently
treated as missing.

## Reading and searching large files

`read_file` and `grep` return a lot of output by default, which is exactly what
fills the context window on a big file. Both accept paging arguments:

```json
{ "path": "src/huge.rs", "offset": 400, "limit": 80 }
{ "pattern": "fn handle", "max_results": 50, "offset": 50 }
```

`read_file` counts **lines** with a 1-based `offset`; when paging is active the
result is prefixed with `[minic] lines 400-479 of 2130` so the model knows it is
not looking at the whole file (without that header it tends to conclude the file
ends there and writes broken code). Omitting both arguments keeps the old
behaviour: the entire file, no header.

`grep` returns `max_results` matches (default 100) and `offset` skips past the
first page; a truncated result ends with `next page: offset=N` so paging is a
copy-paste away.

## MCP servers

Configure servers in `.minic/mcp.json`:

```json
{
  "servers": {
    "demo": { "command": "node", "args": ["server.js"], "env": {} }
  }
}
```

When present, two tools are exposed to the model: `mcp_list` (servers + tools)
and `mcp_call(server, tool, arguments)` (stdio JSON-RPC 2.0). Connecting is
best-effort at startup; `mcp_call` counts as mutating (gated by `--mode`).

## Undo / checkpoints

Before `write_file` / `edit` mutate a file, minic snapshots it under
`.minic/checkpoints/`. Restore the most recent:

```bash
minic --undo               # restore newest checkpoint (consumes it; repeat to walk back)
minic --list-checkpoints
```

Best-effort and git-free for single files. When the workspace is a git
repository and the file already exists, minic additionally stores a
whole-tree snapshot under `refs/minic/*` via `git stash create` (never touches
your index/HEAD); `--undo` restores the whole tree from the newest such ref and
**consumes it** (repeat to walk back through older refs). Untracked files are
handled by the file-based layer (`git stash create` only captures tracked
paths).

### `bash` checkpoints

`bash` is not checkpointed per file — a single command can touch many paths, or
paths outside the jail. Instead, before running a **risky command** (anything
the [shell guard](#shell-guard-bash) would promote to approval: `rm -rf`,
`git push`, `git reset --hard`, `curl | sh`, `sudo`, …) minic stores a
whole-tree snapshot under `refs/minic/cp-tree-*`:

```bash
minic --list-checkpoints      # e.g. git:refs/minic/cp-tree-…  tree  <commit>
minic --undo                  # restores the tree, consumes the ref
```

The tool result tells the model it happened
(`[minic] bash checkpoint … stored (undo restores the tree)`), so a snapshot is
never invisible. This is **best-effort**: it needs a git repo with something to
capture; a non-git workspace reports `bash checkpoint skipped: workspace is not
a git repo` and the command still runs. Disable with `MINIC_CHECKPOINT_BASH=off`.
Note that `--undo` restores the *worktree*, not untracked files created by the
command.

## Sandbox

```bash
minic --sandbox docker --model <m> "…"
minic --sandbox strict --model <m> "…"    # fail-closed if no backend
```

Values: `off` (default), `auto`, `docker`, `bwrap`, `sandbox-exec`, `strict`.
Docker runs ephemeral with `--network none`, `--read-only`, `--cap-drop ALL`,
`--pids-limit 32`, `--memory 512m`, workspace mounted at `/workspace`;
override the image with `MINIC_SANDBOX_IMAGE`. A requested backend that is
unavailable is **fail-closed** — the command is never silently run on the host.

## Interactive TUI

```bash
minic --tui
```

The TUI opens even when no model or API key is configured. Connect a provider
from inside the session:

1. `/connect` (aliases `/provider`, `/login`) opens a small form: base URL,
   then API key (`Tab` switches fields). A blank key keeps the existing one.
2. `/model` fetches `GET {base}/v1/models` and opens a filterable picker.
   Type to filter, `Enter` applies. If the endpoint is unavailable or the list
   is empty, type a model id and press `Enter` to set it manually.
3. Submit a task normally.

You can also preselect a model with `/model <id>` or start with
`minic --tui --model <m>`. Credentials typed into `/connect` stay in memory
for the process only; nothing is written to `.minic/auth.json`. Use
`--save-key` (non-TUI) or environment/config for persistence.

Slash commands:

| Command | Action |
|---|---|
| `/connect`, `/provider`, `/login` | base URL + API key form |
| `/model`, `/models` | fetch and pick a model |
| `/model <id>` | set the model id directly |
| `/discuss`, `/discus`, `/chat` | discuss mode: read-only, no file changes |
| `/execute`, `/exec`, `/build` | execute mode: may modify the workspace |
| `/clear`, `/new` | empty the conversation, keep the session |
| `/help` | show commands and keys |
| `/quit`, `/exit` | leave the TUI |

Typing `/` opens a bare floating list of command names above the composer — no
window, no border, no descriptions, no hint row, no `/` prefix, just the names.
The selected row is shown purely by colour (cyan); there is no arrow marker. It filters as
you type; `↑`/`↓` move the selection, `Tab` or `Enter` fills in the command
without running it, and `Esc` closes the list before clearing the composer. Commands that take an argument
insert a trailing space so you can type the argument straight away. The list
closes as soon as you type a space, because that means you are typing an
argument rather than a command name.

The opening screen is deliberately bare: just the model/context line and the
working directory. Command help lives in `/help`, not on the start screen, so
every session begins without text you have to read past.

### Work modes: discuss and execute

The status line shows the active mode (`model · discuss · ctx 128k`).

- **execute** (default) — the agent may change files and run commands.
- **discuss** — read, search and explain only. This is enforced in the
  permission layer (`PermissionMode::ReadOnly`), not merely requested in the
  prompt: a mutating tool is **denied before it runs**, so the mode cannot
  silently leak the way prompt-only modes do.

Switch with `/discuss` and `/execute` at any point; the change takes effect on
the next tool decision without restarting the session. `--mode readonly` starts
in discuss mode.

### Keys

| Key | Action |
|---|---|
| `Enter` | submit (does nothing while a turn runs) |
| `Ctrl+J` | newline in the composer |
| `↑`/`↓` | history, or move through the autocomplete list |
| `Tab` | complete the command name without running it |
| `Esc` | close the list, else clear the composer, else **stop the running turn** |
| `PgUp`/`PgDn` | scroll the transcript |
| `Enter` on a tool line | open the full tool output; `s` saves it to `.minic/output/` |
| `Ctrl+C` | cancel the turn, or quit when idle |

You can keep typing while a turn runs; `Enter` is ignored until it finishes.

Fullscreen interactive session with a deliberately small visual surface:

- the transcript follows the tail and keeps the newest content above the
  composer; scrolling up (PgUp) pauses follow mode, PgDn returns to the tail;
- the status line is a single dim row: `model · mode · ctx N%` (plus a spinner
  and elapsed time while a turn runs);
- assistant text streams in place; each tool call is one line
  `✓ read_file  src/main.rs  12ms` that updates when the call finishes, with a
  short error preview underneath on failure;
- the composer accepts multiple lines and is wrapped by terminal columns, so
  CJK/emoji input stays aligned.

Keys:

| Key | Action |
|---|---|
| `Enter` | submit |
| `Shift+Enter` / `Ctrl+J` | newline |
| `Up` / `Down` | prompt history |
| `PgUp` / `PgDn` | scroll transcript |
| `Enter` (empty composer) | open the last tool's full output |
| `Esc` | clear input |
| `Ctrl+U` / `Ctrl+K` / `Ctrl+W` | edit line / delete to end / delete word |
| `Ctrl+C` | cancel the running turn; quit when idle |
| `Ctrl+D` | quit when idle |

Bracketed paste is supported. In `--mode ask`, approval is a centered in-TUI
modal that always shows the tool, a one-line summary, the reason, and the
tool's **category**:

| Key | Action |
|---|---|
| `y` | allow this call once |
| `a` | allow this tool for the rest of the session |
| `c` | allow the whole category for the rest of the session (e.g. all `bash`, or all `write_file`+`edit`) |
| `?` | show the full arguments (pretty-printed, scrollable with `PgUp`/`PgDn`, `?`/`Esc` to close) |
| `n` / `Esc` | deny |

Categories group tools by what they risk: `read` (`read_file`, `grep`, `glob`,
`lsp_diagnostics`), `file-write` (`write_file`, `edit`), `shell` (`bash`), and
`other` (`delegate_task`, `mcp_call`). Scope is per session and in-memory only;
`a` narrows to one tool name, `c` widens to the category. If the modal or its
channel disappears, the decision is **deny** (fail-closed).

In non-TUI `--mode ask` the terminal prompt accepts the same letters:
`[y/N/a=this tool/c=all <category>]`. Non-TTY still fails closed.

### Reading a tool's output

The transcript keeps one line per tool call. To read what a tool actually
returned — typically a noisy `bash` command — press `Enter` on an **empty**
composer: a read-only "tool output" panel opens with the last finished call.
`PgUp`/`PgDn` (or `j`/`k`, arrows) scroll it, `Home`/`End` jump, and `Esc`/`q`
closes it. Keystrokes never reach the composer while the panel is open. Output
is capped at 16k characters in the panel; the model always sees the full
result, and `--json` deliberately does not carry it, so the conformance stream
stays compact.

The status line reports scroll position in both directions (`↑ N` for hidden
content above, `↓ N` for content below), so you can tell at a glance that the
transcript extends past the viewport.

Denials are visible, not silent: the transcript and `--json` stream report
every decision (`⊘ bash denied · <reason>`), and the reason distinguishes a
policy denial from a jail denial, a failing pre-hook, and a missing approver.

`--tui` requires a TTY; otherwise minic runs non-interactively with a warning.
A real PTY test verifies alternate-screen entry, one complete turn, and
restoration on exit.

### Ctrl+C semantics

Ctrl+C means two different things depending on whether a turn is running, and
the distinction matters if you are scripting the TUI:

| State | Ctrl+C does |
|---|---|
| turn running (busy) | cancels the turn; TUI stays open, partial text is kept, `cancelled` is noted |
| idle | quits and restores the terminal |

A turn's answer text is rendered *before* the agent reports that the turn
finished, so a Ctrl+C sent the instant an answer appears can land in that
gap and only cancel. Anything driving the TUI (tests, expect scripts) should
keep sending Ctrl+C until the alternate screen is actually left rather than
assuming one keystroke is enough.

## LSP diagnostics

Configure a language server per file extension in `.minic/lsp.json`:

```json
{ "servers": { "rs": { "command": "rust-analyzer", "args": [] } } }
```

The `lsp_diagnostics(path)` tool spawns the server, opens the file, and returns
`publishDiagnostics` as `line [severity] message` lines. The connection is
per-call (spawn → diagnostics → shutdown); unconfigured extensions report a
clear error instead of failing silently.

## Hooks

Pre/post tool hooks are executable scripts under `.minic/hooks/`:

```bash
.minic/hooks/pre_write_file.ps1   # run before write_file
.minic/hooks/post_write_file.cmd  # run after write_file (any tool)
```

Extensions: `.ps1`, `.cmd`, `.bat`, `.sh` (first match wins). The script
receives the tool call as JSON in the `MINIC_HOOK_PAYLOAD` environment
variable: `{"tool":…,"args":…,"cwd":…}` (post hooks also get `"exit_code"`).
A **failing pre-hook blocks the tool** (result marked as error, nothing
mutated). A failing post-hook cannot undo work that already happened, so it
surfaces as a `note` line in the transcript (and a `note` event in `--json`)
rather than being written to stderr, which the TUI's alternate screen would
swallow. Hooks are no-ops when absent.

## Machine-readable output

```bash
minic --model <m> --json "…"     # JSONL events on stdout
```

Envelope: `{"v":1,"event":"…", …}`. Events: `session`, `turn_started`,
`text_delta`, `reasoning_delta`, `tool_call_started`, `tool_decided`,
`tool_call_finished`, `turn_finished`, `agent_finished`, `context_compacted`,
`note`, `error`. Deterministic (used as a conformance surface); diagnostics and
the session id stay on stderr.

`note` carries host warnings that would otherwise only reach stderr and be
swallowed by the TUI's alternate screen — currently a failing post-hook:
`{"event":"note","text":"post-hook: bash post-hook exited with Some(1)"}`.

`tool_decided` is the audit record for the permission layer:
`{"event":"tool_decided","id":…,"name":"bash","decision":"allow|deny|always","reason":…}`.
It is emitted after an `Ask` is resolved and before the tool executes, so a
`deny` explains why nothing happened (policy, jail, hook, or missing approver).

## Sessions

Sessions persist to `.minic/sessions.db` (SQLite). The session id is printed to
stderr at start.

```bash
minic --model <m> "start a task"          # new session
minic --model <m> --resume "continue"     # continue the latest session
minic --model <m> --session my-task "…"   # continue a named session
minic --list-sessions                     # list sessions
```

History is written at **every turn boundary** (and the prompt is saved before
the first model call), so a provider failure or crash mid-run leaves a resumable
session instead of an empty one. In the TUI, the transcript is persisted after
each completed turn (including failed turns).

## API key storage

Prefer storing the key locally rather than in committed config:

```bash
MINIC_API_KEY=sk-... minic --save-key     # writes .minic/auth.json (0600 on Unix)
```

Key resolution order: `--api-key` → `MINIC_API_KEY` → `OPENAI_API_KEY` →
`.minic/auth.json` → `.minic/config.toml`.

## Config (project-local)

Create `.minic/config.toml` in your workspace (safe to commit; keep the key out):

```toml
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
# system = "Optional system prompt override"
shell  = "bash"                          # shell used by the bash tool
# bash_deny_extra = ["terraform apply"]   # always needs approval (shell guard)
# max_subagents = 4                       # parallel delegate_task limit (<= 10)
```

`shell` takes precedence over `MINIC_SHELL`, which takes precedence over the OS
default (`cmd.exe` on Windows, `/bin/sh` elsewhere). A shell whose path contains
`cmd` gets `/C`, anything else gets `-c`. Blank values are ignored so a stray
key cannot break the `bash` tool.

`max_subagents` can only **lower** the hard cap of 10 — raising it is clamped,
so a project config cannot exceed the safety limit.

## Telemetry

minic never sends telemetry over the network. It can write a **local** JSONL
trace, opt-in only:

- `MINIC_TELEMETRY=1` → append events to `.minic/trace.jsonl`
- unset / `0` → disabled (default)

## Test

```bash
cargo test
```

## License

MIT. See [`NOTICE`](./NOTICE) for attribution.
