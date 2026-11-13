# Changelog

All notable changes to minic are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

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
