# minic — Coding Agent Native Rust

> **Rilis: v0.1.0** — 7 fase hardening selesai (lihat bagian akhir).
>
> **Audit mendalam v0.1.0 & rencana perbaikan v0.2.0:**
> [`AUDIT_v0.2.0.md`](./AUDIT_v0.2.0.md) — menemukan 2 bug/gap P0 yang
> diverifikasi empiris (tool call terpotong tetap dieksekusi; tidak ada
> verification loop).
>
> Nama proyek & binary: **`minic`**.
> Repo `minicode` (TypeScript/Bun) **tidak disentuh**; ia hanya dipakai sebagai
> *referensi logika & baseline perilaku*. Tidak ada kode yang disalin.

---

## Status sekarang

**M0–M6 dibangun dan terverifikasi** (Windows, `x86_64-pc-windows-msvc`,
Rust 1.99.1). End-to-end: provider streaming → loop agen → toolset inti →
approval → persistensi → event JSONL → sandbox → checkpoint (file + git
`refs/minic/*`) → MCP → sub-agent → kompaksi (tokenizer BPE) → TUI (PTY) →
LSP → hooks / linear.

**Audit & hardening (M7)**: audit UI/UX + sistem otonom menghasilkan dua
perbaikan yang sudah selesai:

1. **Audit keputusan izin** — `AgentEvent::ToolDecided` (kernel) dipancarkan
   sebelum eksekusi, dirender sebagai `tool_decided` di `--json`, dicatat ke
   `.minic/trace.jsonl` (`tool_decision`), dan ditampilkan menetap di TUI
   sebagai `⊘ <tool> denied · <reason>`. Tanpa ini penolakan tidak bisa
   dibedakan (policy vs jail vs hook vs approver hilang).
2. **Shell guard** — `bash` adalah satu-satunya tool di luar path jail, jadi
   `policy/commands.rs` mengklasifikasi perintah dan **menaikkan perintah
   berisiko ke jalur approval bahkan di `--mode auto`**: destruktif (`rm -rf`,
   `git reset --hard`), publikasi (`git push`, `npm publish`), eksekusi kode
   dari jaringan (`curl | sh`), eskalasi (`sudo`, `runas`), kontrol host
   (`shutdown`, `taskkill`). Non-TTY tetap fail-closed. Override:
   `MINIC_BASH_GUARD=off` dan `bash_deny_extra` di config.
3. **Checkpoint whole-tree untuk `bash`** — perintah berisiko yang lolos guard
   menyimpan snapshot `refs/minic/cp-tree-*` lebih dulu (`checkpoints::snapshot_tree`),
   sehingga `--undo` bisa memulihkan kerusakan shell. Best-effort (butuh repo
   git); hasil tool menyebutkan checkpoint agar tidak tak terlihat.
   Override: `MINIC_CHECKPOINT_BASH=off`.
4. **Approval per scope** — `[y]` sekali, `[a]` tool ini, `[c]` seluruh
   kategori (`read`/`file-write`/`shell`/`other`), `[?]` argumen penuh yang
   bisa digulir, `[n]/Esc` tolak. Berlaku di modal TUI dan prompt linear.
   Penolakan hook kini juga dilaporkan sebagai `ToolDecided{deny}` sehingga
   penyebabnya terlihat, bukan hanya "tool gagal".
5. **Transparansi output & navigasi** — `ToolCallFinished` membawa
   `result_detail` (cap 16k, tidak masuk `--json` agar conformance tetap
   stabil); panel "tool output" baca-saja dibuka dengan `Enter` pada composer
   kosong (`PgUp`/`PgDn`/`Home`/`End`/`Esc`). Status line kini menampilkan
   arah scroll dua arah (`↑ N` / `↓ N`), dan hint composer tidak lagi
   menduplikasi status provider.
6. **Config yang tadinya mati, kini tersambung** — `shell` di
   `.minic/config.toml` dulu dideklarasikan tetapi tidak pernah dibaca;
   sekarang menjadi prioritas pertama di atas `MINIC_SHELL` dan default OS
   (nilai kosong diabaikan). `max_subagents` menurunkan batas pool (clamp ke
   `1..=10`, tidak bisa menaikkan). Tool `read_file` mendapat `offset`/`limit`
   per baris dengan header `[minic] lines a-b of N`, dan `grep` mendapat
   `offset` dengan petunjuk `next page: offset=N`. Post-hook yang gagal tidak
   lagi menulis ke stderr (tidak terlihat di alternate screen) melainkan
   menjadi `AgentEvent::Note` → baris `· …` di transcript, event `note` di
   `--json`, dan `note` di trace.
7. **Rencana tersimpan (M9)** — `delegate_task { plan_id, tasks }` menyimpan
   rencana ke `.minic/plans/<id>.json` dan **tidak** menjalankannya; `--plans`
   dan `--plan-show <id>` meninjau tanpa memanggil model; eksekusi butuh
   `{ plan_id, run: true }` eksplisit. Id rencana dibatasi
   `[A-Za-z0-9_-]{1,64}` karena id menjadi nama berkas — tanpa itu
   `plan_id: "../config"` adalah path traversal yang nyata. Rencana rusak
   dilaporkan sebagai error, bukan dianggap tidak ada; rencana yang gagal
   seluruhnya tetap `draft` agar bisa dicoba lagi. Rencana sengaja **tidak**
   di-gitignore supaya bisa di-commit dan di-diff.

Catatan: kekhawatiran audit soal `render.rs` vs `render/mod.rs` dan
`hooks.rs` vs `hooks_impl.rs` ternyata **bukan duplikasi** — `render.rs` adalah
renderer linear yang memang memiliki submodul `json`/`text`, dan
`hooks_impl.rs` hanyalah adapter tipis trait `AgentHooks`. Tidak diubah, agar
tidak menimbulkan churn tanpa manfaat.

### v0.2.0 — enam fase berikutnya (selesai)

Audit mendalam `AUDIT_v0.2.0.md` menemukan dua bug P0 dan lima kesenjangan
P1–P3. Semuanya sudah diperbaiki, dan tiap perbaikan punya test yang
**terbukti load-bearing** (mutasi balik ke kode lama membuat test gagal).

**A. Integritas respons model (P0).** Dua bug yang saling memperkuat:

1. `agent.rs` membuang `FinishReason` (`ProviderEvent::Finish(_) => {}`), jadi
   respons yang dipotong `max_tokens` tidak bisa dibedakan dari respons
   selesai. Ditambah `serde_json::from_str(&self.args).unwrap_or(Value::Null)`,
   argumen JSON terpotong menjadi `Null` dan tool **tetap dieksekusi** —
   muncul sebagai `bash: missing 'command'`, seolah model yang salah.
   Sekarang: `finish_reason=length` + ada tool call → tool call dibuang,
   teks parsial disimpan **tanpa** tool_call menggantung, dan
   `AgentEvent::Note` menyebut sebabnya. Argumen rusak dilaporkan sebagai
   `malformed tool arguments from model: <serde error>` + potongan argumen
   mentah, **tanpa eksekusi**, dengan `ToolDecided{deny}`. Argumen kosong
   (`Value::Null`) tetap sah untuk tool tanpa parameter.
   `RunOutcome.truncated` menandai kejadian ini.
2. **Bug ketiga ditemukan saat menulis test wire**: adapter
   `openai_compat.rs` selalu mengirim `Finish(Stop)` saat melihat sentinel
   `[DONE]`, sehingga `finish_reason=length` **selalu tertimpa** dan gerbang
   truncation tidak pernah aktif untuk provider nyata. Kini `[DONE]` hanya
   mengisi `Stop` bila stream belum pernah menyebut alasan sebenarnya.

**B. Verification loop (P0).** `--verify <CMD>` / `MINIC_VERIFY` menjalankan
perintah verifikasi lewat tool `bash` (bukan proses sendiri — sandbox, policy
dan checkpoint tetap berlaku) setelah batch tool mutating **berhasil**, lalu
hasilnya **disuntikkan ke history** sebagai tool result. Model melihat error
compiler/test-nya sendiri dan diminta memperbaikinya; kegagalan juga muncul
sebagai `AgentEvent::Note`. Verifikasi tidak jalan bila tidak ada mutasi
(membaca berkas tidak perlu diverifikasi) atau bila `--verify` tidak diisi.
`DEFAULT_SYSTEM` kini menyebut aturan verifikasi eksplisit dan melarang
mengklaim sukses tanpa bukti perintah yang dijalankan.

**C. Pencarian menghormati `.gitignore` (P1).** `IGNORED_DIRS` yang hanya
berisi empat entri hardcoded diganti `common::WalkFilter`: memuat `.gitignore`
bertingkat (batas kedalaman 8) plus `.git/info/exclude`, mendukung negasi
(`!keep.log`), pola ber-anchor (`/foo`), pola dir-only (`dist/`), dan tetap
memaksa `.git`/`target`/`node_modules`/`.minic` diabaikan apa pun isinya.
Dipakai `grep` **dan** `glob`.

**D. Tool read-only paralel (P1).** Loop `for call in calls` yang serial
diganti `batches()`: deretan tool read-only berjalan bersamaan
(`join_all`), sementara tool mutating dan tool tak dikenal tetap serial —
satu modal approval pada satu waktu, dan urutan tulisan tidak boleh ditukar.
Hasil dikembalikan menurut urutan call asli sehingga history deterministik.

**E. Ergonomi biaya & sinyal (P2).** `--max-output-tokens` (dan
`MINIC_MAX_OUTPUT_TOKENS`) diteruskan sampai ke body request provider
(dibuktikan dengan server penangkap, bukan hanya `--help`); nilai 0 diabaikan.
`read_file` mendeteksi biner (NUL di 8 KB pertama) dan menolak dengan pesan
yang jelas, bukan `stream did not contain valid UTF-8`. `grep` mendapat
`context: N` (baris sekitar, `:` untuk match dan `-` untuk konteks seperti
`grep -C`), dibatasi 10 baris.

**F. CI matrix.** `.github/workflows/ci.yml`: gate (fmt + clippy + test) di
Ubuntu/Windows/macOS, job build tanpa fitur opsional, dan smoke test binary
release.

Verifikasi v0.2.0: `cargo fmt --check` bersih, `cargo clippy --all-targets
--all-features -- -D warnings` bersih, `cargo test` → **213 test lulus**
(naik dari 179). Smoke test nyata dengan binary release: agen menulis kode
rusak → `cargo check` gagal → error compiler `E0308` sampai ke model pada
request berikutnya.

Verifikasi v0.1.0 (sebelumnya): `cargo fmt --check` bersih, `cargo clippy --all-targets -- -D
warnings` bersih, `cargo test` → **175 test lulus** (termasuk render smoke test
modal approval & panel output pada ukuran 20×5 hingga 120×40, test runtime
`tests/m7_shell_guard.rs`, test scope kategori, test cap `result_detail`,
`tests/m8_config_wiring.rs` yang membuktikan shell dari config benar-benar
dipakai menjalankan perintah, test paging `read_file`/`grep`, dan
`tests/m9_plans.rs` untuk penyimpanan/tinjauan rencana termasuk penolakan
path traversal).

### Test E2E TUI yang flaky (diperbaiki)

`m6_tui_connect` gagal acak pada "TUI harus memulihkan layar setelah Ctrl+C".
Penyebabnya **bukan** PTY, melainkan semantik Ctrl+C yang memang berbeda: saat
`busy` ia membatalkan turn (TUI tetap terbuka), saat idle ia keluar. Teks
jawaban dirender sebelum `AgentFinished` membersihkan `busy`, sehingga satu
Ctrl+C yang dikirim tepat setelah jawaban terlihat bisa jatuh di jendela itu
dan hanya membatalkan.

Perbaikan (dua lapis):

- **Test** kini mengirim Ctrl+C berulang sampai alternate screen benar-benar
ditinggalkan (menunggu kondisi, bukan `sleep` tetap), plus fallback terakhir
kalau proses keluar sendiri.
- **Kontraknya dikunci** oleh 4 unit test deterministik di
  `tui/app.rs` (`ctrl_c_semantics_tests`), termasuk test yang secara eksplisit
  mereproduksi jendela "teks datang sebelum AgentFinished" — jadi regresi
  terdeteksi tanpa bergantung pada keberuntungan PTY.

Verifikasi tambahan: 3× full suite berturut-turut hijau (179 test).

Upscale reliabilitas (M6-j) juga selesai: persistensi **streaming per-turn**
(plus prompt awal tersimpan sebelum panggilan model pertama), retry provider
429/5xx dengan backoff eksponensial + `Retry-After`, context window otomatis
dari `/v1/models` (`context_length`; `MINIC_CONTEXT_TOKENS` menimpa), registrasi
`lsp_diagnostics` kondisional (hanya bila `.minic/lsp.json` punya server),
`--verbose` untuk reasoning, exit code bermakna (0 sukses, 1 error
provider/transport, 2 batas langkah, 130 abort pengguna), dan Ctrl+C saat
streaming di TUI membatalkan turn tanpa menutup sesi. Verifikasi nyata:
resume lintas proses mengingat konteks ("banana") pada sesi yang sama.

Redesign UI/UX TUI juga selesai: transcript bottom-anchored, status satu baris
redup (`model · mode · ctx N%`), composer multi-baris, tool line ringkas yang
di-update di tempat, modal approval di dalam TUI (`y`/`a`/`n`, fail-closed),
sanitasi ANSI untuk seluruh teks tak tepercaya, history prompt, scroll
PgUp/PgDn, bracketed paste, dan event turn yang dibatalkan tidak bocor lagi ke
UI. Verifikasi: PTY test satu turn penuh + smoke provider nyata di TUI.

Setup model tertunda di TUI juga selesai: `minic --tui` boleh dibuka tanpa
model/API key. `/connect` (alias `/provider`, `/login`) membuka form base URL +
API key (key disamarkan; kosong = pertahankan key lama), lalu `/model`
mengambil `GET {base}/v1/models` dan menampilkan picker yang bisa difilter;
id yang diketik manual tetap bisa dipakai bila endpoint tidak tersedia.
Kredensial dari `/connect` hanya hidup di memori proses. `ModelSession` bersama
membuat turn, picker, dan sub-agent selalu memakai provider/model terbaru, dan
`MINIC_CONTEXT_TOKENS` tetap menimpa `context_length` provider.

```
cargo fmt --check                          → 0 (bersih)
cargo clippy --all-targets -- -D warnings  → 0 (bersih)
cargo test                                 → 111 test lulus
cargo run -- --fake "inspect the project"  → jalan (tanpa jaringan)
```

Contoh output `--fake`:

```
Let me look at the project.        ← dari model (stdout)
  › read_file  Cargo.toml          ← tool mulai (stderr)
    ✓ 0ms                          ← tool selesai (stderr)
Done. (fake provider; 3 messages in context)
· 2 step(s) · 1 tool call(s)       ← ringkasan (stderr)
```

Toolset yang sudah ada: `read_file`, `write_file`, `edit`, `bash`, `grep`,
`glob`. Approval: mode `ask` meminta konfirmasi di TTY, **fail-closed di
non-TTY**. Persistensi: SQLite `.minic/sessions.db` dengan prefix-compare,
`--session`/`--resume`/`--list-sessions`; API key di `.minic/auth.json`.

Struktur yang dibuat:

```
Cargo.toml                # workspace (resolver 3, edition 2024)
rust-toolchain.toml  rustfmt.toml  .gitignore  NOTICE  README.md
crates/core/              # minic-core — kernel murni (tanpa tokio/I/O)
  types.rs events.rs error.rs provider.rs tool.rs permission.rs
  budget.rs tokens.rs history.rs compaction.rs agent.rs
crates/cli/               # minic — host + binary
  main.rs config.rs render.rs render/json.rs render/text.rs sandbox.rs checkpoints.rs subagent.rs model_session.rs telemetry.rs approval.rs auth.rs store.rs hooks.rs hooks_impl.rs lsp.rs
  bin/fake_mcp_server.rs bin/fake_lsp_server.rs
  mcp/{mod,config,client,manager}.rs
  provider/{sse,openai_compat,fake}.rs
  tools/{common,read_file,write_file,edit,bash,grep,glob,mcp,delegate,lsp}.rs
  policy/{mod,jail}.rs
  tui/{mod,app}.rs
  tests/{m0_smoke,m1_tools,m2_permission,m3_store,m4_conformance,m5_app,m6_sandbox,m6_checkpoint,m6_mcp,m6_subagent,m6_compaction,m6_tui_pty,m6_tui_connect,m6_lsp,m6_hooks}.rs
```

---

## 0. Ringkasan

Bangun agen coding terminal dari nol dengan Rust, memakai `minicode` sebagai
peta medan. Target: satu binary statis, startup cepat, memori kecil, dan
aman-by-construction (state machine jadi tipe, resource pakai RAII).

**Prinsip efisiensi** (dipakai untuk setiap keputusan):

1. **Reuse > build.** Jangan tulis SSE parser, TUI, tokenizer, JSON Schema,
   git plumbing, atau template CLI sendiri kalau crate matang ada.
2. **Irisan vertikal, bukan lapisan horizontal.** Satu provider + satu tool +
   output linear harus jalan end-to-end sebelum melebar.
3. **Kernel murni tanpa I/O & tanpa `tokio`.** Logika teruji tanpa jaringan.
4. **Tunda yang tidak membedakan:** daemon, ACP, i18n, RAG, LSP, hooks,
   multi-consumer, HMAC capability.
5. **Oracle conformance**, bukan tebak-tebakan: bandingkan event stream dengan
   replay `minicode exec --json`.

---

## 1. Keputusan (terkunci)

| Area | Pilihan | Catatan |
|---|---|---|
| Nama / binary | `minic` | — |
| Platform primer | **Windows-first** (`x86_64-pc-windows-msvc`) | Linux/macOS menyusul |
| Bahasa | Rust, edition 2024, resolver 3 | — |
| Async | `tokio` (multi-thread) **hanya di tepi** | kernel sinkron |
| HTTP | `reqwest` + `rustls` (tanpa OpenSSL) | — |
| SSE | parser byte sendiri (`provider/sse.rs`) | dipotong hanya pada `\n` → aman fragmentasi UTF-8 |
| CLI | `clap` derive | — |
| JSON | `serde` + `serde_json` | — |
| DB | `rusqlite` (fitur `bundled`) | SQLite statis, tanpa runtime eksternal |
| Sesi | `.minic/sessions.db`, prefix-compare | `--session`/`--resume`/`--list-sessions` |
| TUI | **linear dulu** → `ratatui`+`crossterm` di M5 | — |
| Unicode | `unicode-segmentation`, `unicode-width`, `unicode-properties` — *M5* | ganti `Intl.Segmenter` |
| Git | **shell-out ke `git`** — *M6* | byte-exact |
| Sandbox | Windows: Job Object/AppContainer — *M6*; nonaktif + peringatan sebelum itu | fail-closed bila diminta tapi tak ada backend |
| Secret | `.minic/auth.json` (0600 Unix); config boleh di-commit tanpa key | `--save-key` |
| Config | **project-local** `<root>/.minic/config.toml` | bisa di-commit/di-share per proyek |
| Model | **tanpa default** (wajib kecuali `--fake`) | `--model` → `MINIC_MODEL` → config |
| Shell (`bash`) | **`cmd.exe /C`** di Windows; `MINIC_SHELL` menimpa | `/bin/sh -c` di non-Windows |
| Telemetry | `MINIC_TELEMETRY` (opt-in; **tidak ada** telemetry jaringan) | default nonaktif; `=1` menulis `.minic/trace.jsonl` |
| Distribusi | **cargo-dist** (GitHub Releases + installer) | — |
| Scope | **MVP kecil & bersih** | bukan parity |
| Pace kerja | **full-time (cepat)** | milestone M0–M2 bisa hitungan hari |

### Catatan Windows-first (pelajaran nyata)

- **Path jail:** `fs::canonicalize` menambahkan prefix verbatim `\\?\` di
  Windows, sehingga `starts_with` gagal bila hanya satu sisi dikanonikalisasi.
  Aturan: **kedua sisi** lewat jalur yang sama (canonical bila ada, normalisasi
  leksikal bila tidak). Sudah diterapkan di `policy/jail.rs` + test.
- **Kill-tree** (M1): pakai `taskkill /T /F` (resolve absolut), bukan `kill(-pid)`.
- **Baris baru:** tangani CRLF secara eksplisit; jangan normalisasi diam-diam.
- **Shell** (M1): `cmd.exe /C` sebagai default; deteksi `sh` (git-bash) bila ada.
- **Terminal** (M5): `crossterm`/ConPTY; restore via `Drop` guard, bukan handler
  sinyal manual.

---

## 2. Crate graph

```
crates/core   # minic-core: murni, sinkron, tanpa I/O & tokio
crates/cli    # minic: provider, tools, policy, store, render, binary
```

Aturan batas (di-enforce test): `core` tidak boleh impor `tokio`/`reqwest`.
Arah dependensi satu arah: `cli → core`.

---

## 3. Trait inti (sudah ada di M0)

```rust
pub trait ModelProvider: Send + Sync {
    fn stream(&self, req: StreamRequest) -> BoxStream<'static, Result<ProviderEvent, AgentError>>;
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn execute<'a>(&'a self, args: serde_json::Value, ctx: &'a ToolContext)
        -> BoxFuture<'a, Result<ToolResult, AgentError>>;
}

pub enum Decision { Allow, Deny(String), Ask(String) }
pub trait PermissionHandler: Send + Sync {
    fn decide<'a>(&'a self, tool: &'a str, args: &'a serde_json::Value) -> BoxFuture<'a, Decision>;
}

pub async fn run_agent(provider, tools, permissions, hooks, history, tool_ctx, options, on_event)
    -> Result<RunOutcome, AgentError>;
```

Keputusan tipe yang menghilangkan kelas bug baseline: status/keadaan memakai
`enum` (bukan string) sehingga `match` dipaksa lengkap; id memakai newtype;
error memakai `enum` + `thiserror`; cleanup memakai `Drop` guard.

---

## 4. Milestone

| M | Isi | Kriteria selesai | Status |
|---|---|---|---|
| **M0** | Skeleton, provider OpenAI-compatible + SSE, `read_file`, loop 1-turn, render linear, provider fake | `--fake` jalan; test hijau | ✅ **selesai** |
| **M1** | Tool inti: `write_file`, `edit` (matcher bertingkat), `bash` (cap output, kill-tree `taskkill`, timeout→UNKNOWN), `grep`, `glob`; workspace jail + sensitif | tool berfungsi; jail & policy teruji | ✅ **selesai** |
| **M2** | Permission `auto`/`ask`/`readonly`; prompt approval TTY; **non-TTY fail-closed** | matriks izin hijau; tanpa TTY → deny | ✅ **selesai** |
| **M3** | Persistensi sesi (`rusqlite bundled`), append-only + prefix-compare, `--resume`, auth.json | resume mempertahankan `reasoning`/`is_error` | ✅ **selesai** |
| **M4** | Conformance vs `minicode` (event JSONL) + property test (SSE split-invariance, matcher) | ≥1 skenario cocok; property test hijau | ✅ **selesai** |
| **M5** | TUI (`crossterm`+`ratatui`), transcript + statusline + approval popup | test PTY: boot, turn penuh, restore, Ctrl-C abort | ✅ **selesai** (redesign minimal + approval modal + `/connect`/`/model` deferred setup) |
| **M6+** | MCP (RMCP), checkpoint git, sub-agent, sandbox, LSP, hooks, tokenizer presisi | sesuai permintaan | **M6-a sandbox ✅, M6-b checkpoint ✅ (git `refs/minic/*`), M6-c MCP ✅, M6-d sub-agent ✅, M6-e kompaksi ✅, M6-f PTY ✅, M6-g LSP ✅, M6-h hooks ✅, M6-i tokenizer BPE ✅, M6-j reliabilitas (persistensi streaming, retry, context window, tool kondisional, exit codes) ✅** |

Di pace full-time, M0–M2 realistis dalam hitungan hari; M3–M4 sekitar seminggu;
M5 (TUI) bagian termahal.

---

## 5. Strategi conformance (memakai baseline sebagai oracle)

Ini kunci efisiensi: **jangan menebak perilaku**, salin dari baseline lewat
output terstruktur, bukan dengan membaca kode.

```
minicode exec --json "<prompt>"   (baseline, provider palsu deterministik)
        │  → envelope minicode.output.v1 (JSONL)
        ▼
  normalisasi (buang timestamp/id acak)
        │
        ▼
  fixture conformance ──► minic --json "<prompt>"  (implementasi baru)
                          bandingkan event stream
```

Rencana M4: tambahkan mode `--json` ke `minic`, hasilkan event ternormalisasi,
bandingkan dengan fixture yang dihasilkan dari baseline. Ini memberi ribuan
edge-case perilaku tanpa mem-port test TS.

---

## 6. Non-goals v1

Tidak ada daemon/IPC, ACP, i18n, RAG/embedding, shadow-git. Sandbox hanya bila
ada backend (Windows: Job Object/AppContainer; sebelum itu: nonaktif +
peringatan). LSP/hooks/sub-agent/tokenizer BPE masuk **M6+** (sudah terwujud).
Semua boleh masuk M6+ bila ada permintaan nyata.

---

## 7. Risiko & mitigasi

| Risiko | Mitigasi efisien |
|---|---|
| TUI paling mahal | Ditunda ke M5; `ratatui` dulu; cell-diff custom hanya bila perlu. |
| Async orkestrasi (borrow/Pin) | Kernel sinkron; streaming via channel + `ReceiverStream`, bukan async generator. |
| Unicode edge case (emoji/flags/Thai) | Pakai crate; kunci kasus dari `width.test.ts` baseline sebagai fixture. |
| Kehilangan pengerasan baseline | Conformance oracle + fuzz + fixture. |
| Distribusi | `cargo build --release` + `cargo-dist`; musl static untuk Linux. |
| Windows rewel (ConPTY, `\\?\`) | Sudah mulai ditangani (jail canonicalize); jangan janjikan paritas awal. |
| Scope creep | Milestone punya kriteria selesai; non-goals ditulis. |

---

## 8. Gate (usulan)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --features conformance       # M4, oracle vs fixture
cargo fuzz run sanitize_ansi -- -max_total_time=600
cargo build --release
```

Ambang kualitas ditetapkan, bukan diturunkan agar hijau.

---

## 9. Keputusan yang masih terbuka

Sudah dijawab: nama `minic`, Windows-first, linear-dulu, MVP kecil, skeleton
dibuat, pace full-time. M1: shell `cmd.exe /C`, config project-local `.minic/`,
kill-switch `MINIC_TELEMETRY`, distribusi cargo-dist. M2: model **tanpa
default**, `replace_all` default `false`, cap output 200k & timeout bash 120 s,
`.minic/trace.jsonl` auto-ignore. M3: sesi di SQLite, key di
`.minic/auth.json`.

Yang masih perlu diputuskan nanti (tidak memblokir M4):

1. **Auto-approve** (`--yes`) untuk CI yang tetap ingin mengizinkan mutasi —
   atau biarkan hanya `auto`/config?
2. ~~**Prompt approval**: cukup `y/N` di stderr, atau perlu ringkasan diff/path
   lebih jelas?~~ — sudah dijawab: linear memakai prompt `y/N` + ringkasan
   argumen; TUI memakai modal `y`/`a`/`n` dengan ringkasan tool.
3. ~~**Kompaksi/context window**~~ — sudah dijawab M6-i: tokenizer BPE
   `cl100k_base` (fitur `tiktoken`) + fallback heuristik `chars/4`.

---

## 10. Lisensi & atribusi

Baseline `minicode` berlisensi MIT. Proyek ini juga MIT. Tidak ada kode yang
disalin; atribusi ada di [`NOTICE`](./NOTICE).
