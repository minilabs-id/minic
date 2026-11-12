# Evaluasi Mendalam & Rencana Perbaikan — v0.1.0 → v0.2.0

> Dokumen ini adalah hasil audit kode terhadap `minic` v0.1.0 (179 test, semua
> gate hijau). Tujuannya bukan memuji yang sudah baik, melainkan menunjukkan
> **apa yang membuat agen ini belum bisa dipercaya untuk kerja nyata** — lalu
> merencanakan perbaikannya.

## Ringkasan penilaian

| Dimensi | Nilai | Alasan |
|---|---|---|
| Keamanan & isolasi | **A** | jail, sandbox, fail-closed, audit trail, checkpoint — paling matang dari kebanyakan agen |
| Struktur kode | **A-** | pemisahan kernel/host nyata (core 1.210 LOC tanpa tokio); batas disiplin |
| Test & gate | **A-** | 179 test, fmt+clippy bersih, PTY nyata; tapi **0 test** untuk jalur truncation |
| **Loop inti (kegunaan)** | **D** | tidak memverifikasi hasil kerjanya; tidak menangani respons terpotong |
| Efisiensi | **C** | tool serial; `.gitignore` diabaikan; `max_tokens` tak bisa diatur |
| Observability | **B+** | event stream & trace bagus; tapi kegagalan senyap di jalur truncation |

**Kesimpulan satu kalimat:** fondasi keamanan dan strukturnya sudah kelas atas,
tetapi **loop inti agen belum tertutup** — agen mengubah berkas lalu menyatakan
selesai tanpa pernah membuktikan bahwa hasilnya masih benar.

---

## Bagian 1 — Temuan yang diverifikasi secara empiris

Setiap temuan di bawah ini saya jalankan, bukan hanya dibaca.

### T1 (P0) — `FinishReason` dibuang; tool call terpotong tetap dieksekusi

**Kode:**
```rust
// crates/core/src/agent.rs:228
ProviderEvent::Finish(_) => {}        // ← alasan selesai dibuang

// crates/core/src/agent.rs:141
serde_json::from_str(&self.args).unwrap_or(serde_json::Value::Null)
```

**Bukti (harness nyata, provider mengembalikan `finish_reason=length`):**
```
outcome ok: true
TOOL EXECUTED COUNT = 1
tool result: tool ran with command=<MISSING>
```

Tool **benar-benar dieksekusi** dengan argumen `Value::Null`. Model menerima
`bash: missing 'command'` — pesan yang menyesatkan, karena penyebab sebenarnya
adalah output terpotong. Model lalu mengulang dengan cara sama, gagal lagi,
dan membakar langkah sampai `max_steps`.

**Rantai kegagalan:**
1. server memotong output (`length`, konteks penuh, model kecil)
2. fragmen argumen JSON tidak lengkap
3. `unwrap_or(Null)` menelan kegagalan parse — **tanpa log, tanpa event**
4. tool dieksekusi dengan argumen kosong
5. model disalahkan atas kesalahan transport

**Cakupan test saat ini: 0.** Tidak ada satu pun test menyebut `FinishReason::Length`.

### T2 (P0) — Tidak ada verification loop

- Tidak ada tool yang menjalankan test/lint/typecheck sebagai bagian dari siklus.
- Kernel **tidak pernah** memicu `lsp_diagnostics` setelah `edit`/`write_file`
  (`grep lsp crates/core/src/agent.rs` → kosong).
- `lsp_diagnostics` hanya terdaftar bila `.minic/lsp.json` ada — jadi **mati
  secara default**.
- System prompt tidak menyebut verifikasi sama sekali:
  ```
  "You are minic, a careful terminal coding agent. Use the provided tools to
   inspect and edit the workspace. Prefer reading before editing."
  ```

**Akibat:** agen mengedit, melihat "✓ edit 3ms", lalu menyatakan selesai.
Kompilasi rusak tidak terdeteksi sampai manusia menjalankan build. Ini
perbedaan paling besar antara "demo" dan "bisa dipakai".

### T3 (P1) — `.gitignore` tidak dihormati

```rust
pub const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", ".minic"];
```

**Bukti:** proyek dengan `.gitignore` berisi `dist/`:
```
--- grep output ---
dist/bundle.js:1:NEEDLE in dist      ← seharusnya diabaikan
src/app.rs:1:NEEDLE in src
```

Proyek nyata punya `dist/`, `build/`, `.venv/`, `__pycache__/`, `vendor/`,
`coverage/`. Tanpa parsing `.gitignore`, hasil pencarian memboroskan token dan
menyesatkan model (mis. menemukan "definisi" di bundel hasil build).

### T4 (P1) — Tool dijalankan serial

```rust
// crates/core/src/agent.rs:254
for call in calls {
```

3 `read_file` paralel dari model = 3 round-trip berurutan. `Tool` adalah
`Send + Sync` dan `is_mutating()` default `false`, jadi paralelisasi aman
secara mekanis untuk `read_file`/`grep`/`glob`/`lsp_diagnostics`.

**Kendala yang harus dihormati:** `Decision::Ask` (approval interaktif) tidak
bisa diparalelkan — satu modal pada satu waktu. Jadi paralelisasi hanya boleh
untuk batch yang **semuanya** Allow tanpa approval.

### T5 (P2) — `max_tokens` tidak bisa diatur

`crates/cli/src/main.rs:426` selalu `max_tokens: None`. Provider mendukungnya
(`body["max_tokens"] = json!(max_tokens)`). Tidak ada flag, tidak ada env.
Konsekuensi: biaya output per panggilan tidak bisa dibatasi, dan T1 lebih
mudah terjadi (output panjang dipotong server tanpa batas yang jelas).

### T6 (P2) — Kegagalan parse argumen senyap

Berkaitan dengan T1, tetapi lebih umum: `unwrap_or(Null)` dipakai untuk
**semua** kegagalan parse, bukan hanya truncation. Argumen yang salah bentuk
karena alasan apa pun berubah menjadi "tool dipanggil tanpa argumen" alih-alih
"argumen rusak: <alasan>".

### T7 (P3) — Kesenjangan ergonomi

| Item | Dampak |
|---|---|
| `grep` tanpa `context: N` | model harus `read_file` tambahan untuk melihat sekitar |
| tidak ada tool `todo` | tugas multi-langkah tanpa rencana persisten |
| `read_file` tidak deteksi biner | pesan io mentah, bukan "berkas biner" |
| kompaksi lossy tanpa LLM | keputusan arsitektur hilang di sesi panjang |
| tidak ada CI (`.github/`) | regresi lintas-platform tidak tertangkap |

---

## Bagian 2 — Rencana perbaikan

Diurutkan menurut **dampak ÷ usaha**. Setiap fase punya gate yang sama seperti
fase sebelumnya: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` hijau.

### Fase A — Integritas respons model (T1 + T6) · P0 · Kecil

Target: **tidak ada tool yang pernah dieksekusi dari respons terpotong**, dan
kegagalan parse selalu terlihat.

1. `crates/core/src/provider.rs` — tidak perlu berubah (`FinishReason` sudah ada).
2. `crates/core/src/agent.rs`:
   - tangkap `ProviderEvent::Finish(reason)` ke `let mut finish_reason`.
   - bila `reason == Length` **dan** ada `calls`: jangan eksekusi satu pun.
     - buang `calls`, dorong `Message::assistant_with(text, vec![])`.
     - pancarkan `AgentEvent::Note`:
       `"response truncated (finish_reason=length); tool calls discarded — raise max_tokens or split the task"`.
     - `break` keluar dari loop dengan outcome yang menandai truncation.
   - `into_tool_call`: bedakan "kosong" dari "rusak".
     - args kosong → `Value::Null` (sah; tool tanpa parameter).
     - args tidak kosong tapi gagal parse → kembalikan `None` beserta alasan,
       dan laporkan sebagai `ToolResult::err` **tanpa eksekusi**, dengan pesan
       `"malformed tool arguments from model: <serde error>"`.
3. `crates/core/src/events.rs` — tambahkan `truncated: bool` ke `AgentFinished`
   (atau event `Note` saja; dipilih `Note` agar schema `--json` tetap stabil).
4. `crates/cli/src/render.rs` + `render/json.rs` + `tui/app.rs` — `Note` sudah ada.

**Test wajib (semuanya baru):**
- `finish_length_discards_tool_calls` — provider mengembalikan `Length` +
  fragmen terpotong; **assert tool tidak pernah dieksekusi** (spy counter = 0),
  dan `Note` terpancar.
- `finish_stop_executes_normally` — kontrol, memastikan perbaikan tidak
  memblokir jalur normal.
- `malformed_arguments_are_reported_not_nulled` — args `"{bad"` → hasil error
  menyebut "malformed", tool tidak dieksekusi.
- `empty_arguments_still_allowed` — tool tanpa parameter tetap jalan.

**Gate tambahan:** harness `probe` yang membuktikan bug harus berubah jadi
`TOOL EXECUTED COUNT = 0`.

### Fase B — Verification loop (T2) · P0 · Sedang

Target: agen **tahu** ketika hasil kerjanya rusak, tanpa manusia.

1. **System prompt** (`crates/cli/src/main.rs`, `DEFAULT_SYSTEM`): tambahkan
   aturan eksplisit:
   - "After changing code, verify it: run the project's test/typecheck command
     with `bash` (e.g. `cargo check`, `npm test`). Do not claim a task is done
     without evidence that it compiles/passes."
2. **`--verify <cmd>`** (flag baru) + `MINIC_VERIFY`:
   - setelah setiap batch yang berisi tool mutating dan berhasil, jalankan
     perintah verifikasi lewat `bash`.
   - hasilnya **disuntikkan ke history sebagai tool result** (bukan hanya
     dicetak), agar model melihat dan memperbaiki sendiri.
   - gagal verifikasi → tidak menggagalkan turn; model yang memutuskan.
   - dokumentasikan biaya: satu extra step per batch mutasi.
3. **LSP otomatis** (opsional, bila `.minic/lsp.json` ada):
   - setelah `edit`/`write_file` sukses, panggil `lsp_diagnostics` untuk path itu.
   - hanya diagnostik `error` (bukan warning) yang disuntikkan, agar tidak
     membanjiri konteks.

**Test wajib:**
- `verify_runs_after_mutating_tool` — edit sukses → perintah verifikasi
  dijalankan dan hasilnya muncul di history.
- `verify_skipped_for_read_only_batch` — `read_file` saja → tidak ada verifikasi.
- `verify_failure_is_visible_to_model` — verifikasi exit≠0 → model melihat
  outputnya (bukan hanya stderr).
- `verify_does_not_run_on_denied_tool` — tool ditolak → tidak ada verifikasi.

### Fase C — `.gitignore` & efisiensi pencarian (T3) · P1 · Kecil

1. `crates/cli/src/tools/common.rs`:
   - parse `.gitignore` (root + nested) sekali per tool call.
   - tetap hormati `IGNORED_DIRS` sebagai fallback bila tidak ada `.gitignore`.
   - tangani pola dasar: `dir/`, `*.ext`, `!negasi`, `/anchor`.
   - tambahkan `*.git`-style glob via `globset` yang **sudah** jadi dependency.
2. Terapkan di `grep` dan `glob` (`filter_entry`).
3. Hormati `.git/info/exclude` bila ada (murah, menghindari kejutan).

**Test wajib:**
- `grep_skips_gitignored_dirs` — reproduksi persis probe di atas (`dist/`
  tidak muncul).
- `negation_pattern_is_honoured` — `!keep.log` tetap dicari.
- `no_gitignore_falls_back_to_defaults` — perilaku lama tidak berubah.
- `nested_gitignore_applies` — `.gitignore` di subdir.

### Fase D — Eksekusi paralel tool read-only (T4) · P1 · Sedang

1. `crates/core/src/agent.rs`: partisi `calls` menjadi batch berurutan:
   - batch = rangkaian tool yang **semua** `!is_mutating()` **dan** keputusan
     awalnya bukan `Ask`.
   - batch panjang > 1 → `futures_util::future::join_all`.
   - `Ask` dan semua tool mutating tetap serial, urutan dipertahankan.
2. Event: `ToolCallStarted` untuk semua anggota batch dulu, lalu
   `ToolCallFinished` sesuai urutan **asli** (bukan urutan selesai) agar output
   deterministik.
3. Hasil didorong ke history **dalam urutan asli** (penting: tool-pairing valid).

**Test wajib:**
- `parallel_batch_preserves_result_order` — 3 read yang selesai terbalik →
  history tetap urut asli.
- `mutating_tools_stay_serial` — urutan edit→edit dipertahankan.
- `ask_breaks_parallel_batch` — satu `Ask` di tengah → batch dipecah.
- `parallel_does_not_change_single_call_behaviour`.

### Fase E — Pengaturan biaya & pesan biner (T5 + T7 sebagian) · P2 · Kecil

1. `--max-output-tokens <n>` + `MINIC_MAX_OUTPUT_TOKENS` → isi
   `RunOptions.max_tokens` (sekarang selalu `None`).
2. `read_file`: deteksi NUL byte di 8KB pertama → error jelas
   `"binary file (N bytes); use bash/xxd to inspect"`.
3. `grep`: tambahkan `context: N` (baris sebelum/sesudah, format `-`/`+`).

**Test wajib:** `max_output_tokens_reaches_provider`,
`binary_file_is_detected`, `grep_context_includes_surrounding_lines`.

### Fase F — CI lintas platform (T7) · P2 · Kecil

`.github/workflows/ci.yml`: matrix `windows-latest` + `ubuntu-latest`,
`cargo fmt --check`, `clippy -D warnings`, `cargo test`. Ini yang akan
menangkap regresi `sandbox` Unix yang saat ini tidak pernah dijalankan.

---

## Bagian 3 — Urutan & estimasi

| Fase | Temuan | Prioritas | Perkiraan usaha | Risiko regresi |
|---|---|---|---|---|
| A | T1, T6 | P0 | Kecil (1 file inti + test) | Rendah |
| B | T2 | P0 | Sedang (kernel + CLI + prompt) | **Sedang** (mengubah alur) |
| C | T3 | P1 | Kecil (1 modul) | Rendah |
| D | T4 | P1 | Sedang (kernel) | **Sedang** (urutan & event) |
| E | T5, T7 | P2 | Kecil | Rendah |
| F | T7 | P2 | Kecil | Nol |

**Rekomendasi urutan: A → C → E → B → D → F.**
Alasan: A,C,E kecil dan langsung menutup lubang; B mengubah alur (butuh
perhatian penuh); D menyentuh kernel paling dalam sehingga paling aman
dikerjakan setelah B stabil.

---

## Bagian 4 — Yang **tidak** akan diubah (dan alasannya)

Penting untuk tidak menimbulkan churn tanpa manfaat:

- **Kompaksi lossy** — ringkasan berbasis LLM menambah biaya dan
  nondeterminisme; dokumentasi sudah jujur soal sifat lossy. Ditunda.
- **Tool `todo`** — `plans.rs` sudah cukup untuk kebutuhan nyata; menambah
  tool baru hanya menambah token skema.
- **`render.rs` vs `render/mod.rs`, `hooks.rs` vs `hooks_impl.rs`** — sudah
  diverifikasi **bukan** duplikasi (renderer linear memiliki submodul; hooks_impl
  adalah adapter trait). Tidak disentuh.
- **Self-termination adaptif** — tidak akan diimplementasikan tanpa persetujuan
  eksplisit; risiko agen memutuskan sendiri kapan berhenti terlalu besar.

---

## Bagian 5 — Definition of done untuk v0.2.0

- [ ] Harness truncation lama menghasilkan `TOOL EXECUTED COUNT = 0`
- [ ] `--verify` terbukti menyuntikkan hasil ke history (test, bukan asumsi)
- [ ] `grep` tidak lagi mengembalikan hasil dari direktori yang di-.gitignore
- [ ] Batch read-only paralel dengan urutan hasil deterministik
- [ ] `--max-output-tokens` sampai ke provider
- [ ] CI hijau di Windows **dan** Linux
- [ ] Semua gate bersih; jumlah test naik dari 179 → ±200
- [ ] Tidak ada perubahan perilaku pada jalur fail-closed
