# Changelog

All notable changes to minic are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] — 2026-11-13

Closes every finding from the work-mode audit (`AUDIT_work_mode.md`, W1–W10).
Each fix is guarded by a test that was proven load-bearing by mutation: revert
the code and the test fails.

### Security

- **Verification no longer bypasses the permission handler** (W1). `run_verify`
  called `execute_tool` directly, so `--verify` ran shell commands without an
  approval prompt in `ask` mode and skipped the shell-risk guard. It now goes
  through `permissions.decide`, and the `ToolDecided` event reports the real
  decision instead of the false `Always`. A denial surfaces to the model as a
  `permission denied` result plus a `verification NOT RUN` note, never as
  "passed".
- **Approval categories are no longer over-broad** (W8). `mcp_call`, `mcp_list`
  and `delegate_task` shared `Category::Other`, so pressing `c` on one MCP call
  also granted delegation. They now have `Category::Mcp` and
  `Category::Delegate`. Unmapped tools stay `Other` and cannot be granted as a
  category at all — the approval prompt stops offering `[c]` for them.
- **Switching modes discards "always allow" grants** (W9). A grant made in
  `execute` no longer survives into `discuss`, so returning to a mode re-asks
  instead of silently reusing a decision made under different rules.

### Added

- **The model now knows the active mode** (W3, W4). `WorkMode::system_note()` is
  injected into the system prompt each turn, and every switch appends a system
  message to the model's history. Previously the mode was enforced but never
  communicated, so the agent kept attempting denied tools and looked broken.
- **The mode persists across sessions** (W5). A new `mode` column on the
  `sessions` table (additive migration) is restored on start unless `--mode` is
  given explicitly. An unrecognised stored value is ignored rather than treated
  as `execute`.
- **Sub-agents inherit the parent's mode** (W7). `SubAgentPool::with_policy`
  makes delegation follow the caller: `execute` lets a sub-agent register
  `write_file` and `edit`; `discuss` keeps it read-only. Sub-agents still get no
  `bash` and no nested `delegate_task`.
- **Mode switching mid-turn is explained** (W6). The transcript states that the
  change applies from the next tool call and that `Esc` stops the running turn.

### Fixed

- **The mode label can no longer be truncated** (W10). It was rendered inside
  the truncatable status string; on narrow screens a user in `discuss` could read
  `discus` and assume `execute`. It now has its own fixed-width column that
  shrinks only after the path/hint column.
- **`set_mode` stores `to_u8()` instead of `mode as u8`**, so adding a variant can
  no longer shift the stored value silently.

### Removed

- **`ToolContext.permission_mode`** (W2). It was written once at startup and read
  by nothing; with modes now switchable it was stale and misleading. The
  `PermissionHandler` is the single source of truth, read per decision.

## [0.3.0] — 2026-11-13

### Added

- **Work modes: `/discuss` and `/execute`.** `discuss` is read-only: mutating
  tools are **denied in the permission layer** (`PermissionMode::ReadOnly`)
  before they run, so the mode cannot leak the way prompt-only modes do.
  `execute` is read-write and is the default. Switch at any point with
  `/discuss` (`/discus`, `/chat`) and `/execute` (`/exec`, `/build`) without
  restarting the session; `--mode readonly` starts in discuss mode. The status
  line shows the active mode.
- **`/clear`** (alias `/new`) — empty the conversation and the model history
  while keeping the session (provider, model, working directory).
- **Floating command autocomplete.** Typing `/` opens a bare list of command
  names above the composer — no window, no border, no descriptions, no `/`
  prefix. The selected row is shown purely by colour. `↑`/`↓` move, `Tab` or
  `Enter` fill in, `Esc` closes.
- **Save tool output.** In the tool-output panel, `s` writes the body to
  `.minic/output/<tool>-<n>.txt`. Tool names are stripped of path characters
  and files are numbered so earlier saves are never overwritten.
- Verification loop: `--verify <CMD>` / `MINIC_VERIFY` runs after a successful
  mutating batch and feeds failures back to the model.
- `--max-output-tokens` flag and `MINIC_MAX_OUTPUT_TOKENS` environment variable.
- `grep` gained a `context: N` parameter; `read_file` gained `offset`/`limit`.
- Parallel execution of consecutive read-only tool calls, with results always
  returned in call order.
- CI matrix (Linux, Windows, macOS) running fmt, clippy `-D warnings` and the
  test suite, plus a `--no-default-features` build and a release smoke test.

### Changed

- The opening screen no longer prints command explanations. It shows only the
  model/context line and the working directory; help lives in `/help`.
- `ctx 0%` on an empty session is now `ctx 128k` (capacity), switching to a
  percentage once there is content.
- The composer can be edited while a turn is running; `Enter` is held back
  until the turn finishes.
- `Esc` is now layered: close the autocomplete list, then clear the composer,
  then **stop the running turn**.
- `search` and `glob` honour `.gitignore` (including nested files and
  `.git/info/exclude`), with negation, anchoring and directory-only rules.
- `read_file` rejects binary files with an actionable message instead of a raw
  UTF-8 error.

### Removed

- **`/plan` and `/verify` slash commands.** Both were listed in the
  autocomplete and `/help` but `run_command` never executed them, so pressing
  Enter produced "unknown command". A feature the UI advertises but does not
  perform is worse than no feature. Saved plans remain available through
  `--plans` and `--plan-show`, and `--verify` remains a flag.
- `/model <id>` as a distinct command entry is still supported through `/model`
  with an argument.

### Fixed

- `[DONE]` sentinel in the OpenAI-compatible stream no longer overwrites a real
  `finish_reason`, which had silently broken the truncation gate.
- `finish_reason=length` with pending tool calls now discards those calls and
  saves the partial text, instead of persisting a message with dangling
  `tool_calls`.
- Malformed tool arguments are reported to the model without being executed.
- The autocomplete list could draw outside the buffer (panic `index outside of
  buffer`) on short terminals once the command count grew; its height is now
  clamped to the space above the composer.
- `fase_d_parallel::read_only_calls_run_concurrently` failed intermittently
  (~25%) because it asserted a wall-clock bound. Concurrency is proven by the
  peak-concurrency counter instead.

### Security

- Risky shell commands are promoted to an approval prompt rather than run
  silently; non-TTY sessions fail closed.
- Shell commands are checkpointed into `refs/minic/cp-tree-*` when the
  workspace is a git repository.
- Approvals are category-scoped (`y` once, `a` this tool, `c` whole category,
  `n` deny, `?` full arguments) and the final decision is recorded in the
  event stream, JSON conformance output and telemetry.

## [0.1.0]

First tagged release: kernel/CLI split, 179 tests, streaming session
persistence, provider retry/backoff, context-window detection, conditional tool
registration, and meaningful exit codes.
