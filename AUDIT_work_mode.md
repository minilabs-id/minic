# Audit: Work Mode (`/discuss` dan `/execute`)

**Tanggal:** 2026-11-13
**Ruang lingkup:** `WorkMode` di `crates/cli/src/tui/app.rs`, `SimplePolicy::set_mode`
di `crates/cli/src/policy/mod.rs`, wiring TUI di `crates/cli/src/tui/mod.rs`, dan
interaksi dengan loop agent di `crates/core/src/agent.rs`.

**Ringkasan:** penegakan mode di lapisan izin **benar dan terbukti** — mutating
tool ditolak sebelum dijalankan, dan itu sudah dijaga test end-to-end. Masalahnya
bukan pada penegakan, melainkan pada **apa yang model dan pengguna ketahui** serta
**kebocoran di jalur lain**. Ditemukan 4 kebocoran nyata (W1–W4) dan 6 kekurangan
desain (W5–W10).

> **Status: SEMUA TEMUAN SUDAH DIPERBAIKI** (v0.3.1). Tiap perbaikan dijaga test
> yang dibuktikan *load-bearing* lewat mutasi (kode dikembalikan → test gagal).
> Lihat tabel di Bagian 3 untuk ringkasan dan test penjaganya.

---

## Bagian 1 — Kebocoran (harus diperbaiki)

### W1. `run_verify` menjalankan `bash` tanpa melewati handler izin — SERIUS

`crates/core/src/agent.rs:551` `run_verify()` memanggil `execute_tool()` langsung:

```rust
let mut result = execute_tool(tools, &call, tool_ctx).await;
```

Tidak ada `permissions.decide(...)` di jalur ini. Konsekuensi:

- Pada mode `execute` + `--mode ask`, perintah verifikasi **tidak pernah** meminta
  approval meskipun `bash` adalah tool mutating. Pengguna yang memilih "ask"
  mengira setiap perintah shell akan dikonfirmasi; perintah verifikasi lolos
  diam-diam.
- Shell guard (`shell_risk`) juga dilewati: `--verify "rm -rf /"` akan dijalankan
  tanpa pemeriksaan risiko.
- Mode `discuss` saat ini **tidak** terkena, karena `mutated` hanya bisa `true`
  bila ada `Allow` nyata — dan di discuss semua mutasi ditolak. Jadi ini bukan
  kebocoran discuss, tetapi **lubang yang sama jenisnya** yang akan menjadi
  masalah begitu `execute` disandingkan dengan `ask`.

**Dibuktikan secara eksperimental, bukan hanya dari membaca kode:**
`crates/cli/tests/probe_w1_verify_bypass.rs` memasang handler izin yang menolak
`bash`, lalu menjalankan satu mutasi sukses + `--verify`. Hasilnya test lulus
dengan dua assertion: `bash` **benar-benar dijalankan**, dan handler izin
**tidak pernah dipanggil**. Ketika W1 diperbaiki, test itu harus diubah menjadi
kebalikannya (`assert!(!ran)`), sehingga perbaikannya sendiri terjaga.

Perbaikan: jalankan verifikasi lewat `permissions.decide` yang sama seperti tool
biasa, dan emit `ToolDecided` dengan keputusan sebenarnya (sekarang selalu
`DecisionKind::Always` — itu **klaim palsu** di jejak audit).

> **✅ DIPERBAIKI.** `run_verify` sekarang memanggil `permissions.decide` dan
> mengembalikan `(result, DecisionKind, reason)`; `ToolDecided` melaporkan
> keputusan sebenarnya. Penolakan menjadi hasil tool "permission denied" plus
> `Note` "verification NOT RUN", bukan klaim lulus. Penjaga:
> `probe_w1_verify_bypass.rs` (assertion sudah dibalik) + test `fase_b_verify`.

### W2. `tool_ctx.permission_mode` adalah state mati yang menyesatkan

`ToolContext.permission_mode` diisi sekali saat startup
(`crates/cli/src/main.rs:376`) dan **tidak pernah dibaca oleh tool mana pun**
(sudah saya cek: nol pembacaan `.permission_mode` di seluruh `crates/`).

Bahayanya bukan runtime, tapi desain: field ini terlihat seperti sumber kebenaran
mode, sehingga orang berikutnya bisa menulis tool yang mempercayainya dan
mendapatkan nilai basi setelah `/discuss`. Setelah `/discuss`, field itu tetap
berkata `auto`.

Perbaikan: hapus field (atau isi dari `policy.mode()` setiap turn, lalu dokumentasi
tegas bahwa ia **snapshot**, bukan mode hidup). Menghapus lebih jujur.

> **✅ DIPERBAIKI.** Field dihapus dari `ToolContext` beserta semua konstruksinya.
> Satu-satunya sumber kebenaran adalah `PermissionHandler`, yang dibaca per
> keputusan.

### W3. Model tidak pernah diberi tahu mode aktif

`spawn_turn()` di `crates/cli/src/tui/mod.rs` tidak menyentuh `app.work_mode()`
sama sekali (sudah saya verifikasi). System prompt dari `DEFAULT_SYSTEM` juga
tidak menyebut mode.

Sudah dipastikan statis: `spawn_turn` hanya melakukan `deps.system.clone()`
(`tui/mod.rs:528`) dan `DEFAULT_SYSTEM` di `main.rs` tidak memuat kata "discuss"
maupun "mode" sama sekali.

Akibatnya, di mode diskusi model tetap mencoba `write_file`/`edit`/`bash`,
mendapat `permission denied: readonly mode: mutating tool denied`, lalu
mungkin mencoba lagi dengan tool lain — membuang langkah dan token, dan di
permukaan tampak seperti agent yang rusak padahal ia hanya tidak diberi tahu.

Perbaikan: suntikkan satu baris ke system prompt sesuai mode, mis.

- discuss: *"You are in discuss mode: you cannot modify files. Read, search and
  explain; propose changes as text or a patch, do not attempt to apply them."*
- execute: *"You are in execute mode: you may modify the workspace."*

Hasil yang diharapkan: penolakan jarang terjadi, bukan hanya aman.

> **✅ DIPERBAIKI.** `WorkMode::system_note()` disuntikkan ke system prompt di
> `spawn_turn`. Test: `system_prompt_carries_the_mode_note`,
> `discuss_mode_tells_the_model_it_cannot_write`.

### W4. Mode tidak pernah masuk ke riwayat percakapan

`set_work_mode` hanya memanggil `push_note`, yang menulis ke `transcript` —
bukan ke `messages`. Jadi pergantian mode **tidak terlihat oleh model** di
turn berikutnya meskipun prompt sistem tidak diubah. Bila pengguna menulis
"lanjutkan" setelah `/discuss`, model tidak punya konteks bahwa ia kini
read-only.

Perbaikan: bersama W3, atau tambahkan catatan sistem berumur pendek di depan
pesan user pertama setelah pergantian mode.

---

> **✅ DIPERBAIKI.** `set_work_mode` menambahkan `Message::system("[mode changed] …")`
> ke `messages`. Test: `switching_mode_records_itself_in_model_history`.

## Bagian 2 — Kekurangan desain

### W5. Mode tidak bertahan antar sesi

`--resume` / `--session` memulihkan pesan tetapi tidak mode. Pengguna yang
bekerja di mode diskusi harus mengetik `/discuss` lagi setiap kali membuka
sesi. Untuk mode yang berfungsi sebagai *guardrail*, default yang berubah diam-diam
ke `execute` adalah default yang salah.

Perbaikan: simpan mode di `SessionInfo` (sudah ada di `store.rs`) atau di
`config.toml`, dan pulihkan saat resume. `--mode` eksplisit tetap menang.

> **✅ DIPERBAIKI.** Kolom `mode` di tabel `sessions` (migrasi aditif);
> dipulihkan saat start kecuali `--mode` diberikan eksplisit. Label tak dikenal
> → `None` (tidak pernah naik ke eksekusi). Test: `work_mode_survives_a_restart`,
> `work_mode_is_per_session`, `unknown_stored_mode_is_ignored_rather_than_escalated`.

### W6. `SetMode` bisa terjadi di tengah turn, efeknya tidak terduga

`SimplePolicy` membaca mode **per keputusan**. Artinya bila pengguna menekan
`/discuss` saat agent sedang di langkah ke-3 dari 8, langkah 4 dan seterusnya
tiba-tiba ditolak. Ini bisa jadi fitur (rem darurat) — tetapi sekarang tidak
diniatkan maupun didokumentasikan, dan `app.work_mode()` yang ditampilkan di
status line **berbeda** dari mode yang dipakai turn yang sedang berjalan
sampai keputusan berikutnya.

Perbaikan: tentukan semantiknya secara eksplisit. Saran: perubahan mode berlaku
untuk keputusan **berikutnya** (seperti sekarang), tetapi beri catatan di
transcript: *"applies from the next tool call; the running turn may still finish
its current step"*. Dan bila pengguna ingin rem darurat, arahkan ke `Esc`
(membatalkan turn) alih-alih mode.

> **✅ DIPERBAIKI (sebagian, sesuai desain).** Semantik "berlaku untuk tool
> berikutnya" dipertahankan, tetapi sekarang **dikatakan** di transcript saat turn
> sedang berjalan, beserta petunjuk `Esc` untuk menghentikannya.

### W7. `discuss` tidak mencegah `delegate_task`

`delegate_task` ditandai `is_mutating() == true`, jadi di discuss ia ditolak.
Itu benar. Namun subagent yang dijalankan di mode `execute` selalu memakai
`PermissionMode::ReadOnly` (`subagent.rs:97`) — jadi **tidak ada cara** membuat
subagent yang boleh menulis. Fitur "delegate" praktis hanya bisa membaca,
selamanya. Ini bukan kebocoran, tapi batasan yang tidak terdokumentasi dan
membuat `delegate_task` kurang berguna daripada yang terlihat.

Perbaikan: teruskan mode induk ke subagent, atau dokumentasikan tegas bahwa
subagent selalu read-only.

> **✅ DIPERBAIKI.** `SubAgentPool::with_policy` mewariskan policy induk;
> `may_write()` menentukan apakah `write_file`/`edit` didaftarkan. Induk diskusi →
> delegasi read-only. Test: `subagent_inherits_the_parent_work_mode`.

### W8. Kategori approval menggabungkan tool yang tidak sejenis

`Category::of_tool` memetakan semua nama tak dikenal ke `Category::Other`.
Akibatnya `mcp_call`, `mcp_list`, dan `delegate_task` berbagi satu kategori:
menekan `c` ("izinkan seluruh kategori") pada `mcp_call` juga mengizinkan
`delegate_task` dan setiap MCP call lain, sekarang dan di masa depan.

Perbaikan: beri `Category::Mcp` dan `Category::Delegate` sendiri, dan jangan
sertakan `Other` dalam tombol "whole category" — kategori `Other` sebaiknya
memaksa `Tool(..)` saja.

> **✅ DIPERBAIKI.** `Category::Mcp` dan `Category::Delegate` dipisah; `Other`
> tidak lagi *grantable* (`is_grantable()`), dan modal tidak menawarkan `[c]`
> untuknya. Test: `mcp_and_delegate_do_not_share_a_category`,
> `unknown_tools_are_not_grantable_as_a_category`.

### W9. `set_mode` tidak menyentuh set `always` yang sudah diberikan

Bila pengguna pernah menekan `a` untuk `write_file` di mode execute, lalu
berpindah ke discuss dan kembali ke execute, izin itu masih berlaku. Secara
perilaku aman (discuss tetap menolak karena penolakan terjadi lebih dulu), tapi
secara mental membingungkan: "saya sudah keluar dari mode itu, kok masih tidak
ditanya?"

Perbaikan: biarkan (dan dokumentasikan), atau bersihkan set `always` saat mode
berubah ke arah yang lebih permisif, supaya pengguna menyetujui ulang.

> **✅ DIPERBAIKI.** `set_mode` mengosongkan set `always` dan memakai `to_u8()`
> (bukan `as u8`). Test: `set_mode_discards_always_grants`,
> `mode_round_trips_through_the_atomic`.

### W10. Label mode bersaing dengan informasi lain di satu baris

`model · discuss · ctx 128k`. Di layar sempit, `truncate(&sanitize(app.status()), 60)`
bisa memotong bagian mana pun. Karena mode adalah state keselamatan, ia tidak
boleh bisa terpotong. Sekarang ia bisa.

Perbaikan: tampilkan mode di kolom tetap (mis. awal baris, atau badge kanan),
bukan di tengah string yang dipotong.

---

> **✅ DIPERBAIKI.** Mode dirender di kolom `Length` tetap, di luar string yang
> dipotong; kolom kanan yang mengalah lebih dulu. Test:
> `mode_label_is_visible_even_on_a_narrow_screen` (lebar 40/30/24/20).

## Bagian 3 — Yang sudah benar (jangan diubah)

- **Penegakan di policy, bukan prompt.** Ini keputusan terbaik di seluruh fitur
  dan alasannya sudah ditulis di komentar. Riset agent lain (OpenCode) menunjukkan
  mode berbasis prompt bocor.
- **`from_u8` jatuh ke `Ask`, bukan `Auto`.** Fail-safe yang benar.
- **`Arc<AtomicU8>` + baca per keputusan.** Membuat pergantian mode hidup tanpa
  restart, tanpa `&mut`.
- **`mutated` mensyaratkan `Allow` nyata.** Ini yang mencegah verifikasi berjalan
  di discuss.
- **Test end-to-end berpasangan** (discuss menolak *dan* execute menulis).
  Tanpa pasangan itu, test discuss bisa lulus palsu.
- **Alias** `/discus`, `/chat`, `/exec`, `/build` — murah dan membantu.

---

## Bagian 4 — Urutan perbaikan yang disarankan

| Prioritas | Item | Alasan |
|---|---|---|
| 1 | **W1** verifikasi lewat handler izin | Kebocoran nyata; klaim `DecisionKind::Always` di jejak audit tidak benar |
| 2 | **W3 + W4** mode masuk system prompt | Menghilangkan penolakan yang bisa dihindari; membuat mode terasa disengaja |
| 3 | **W2** hapus `tool_ctx.permission_mode` | State menyesatkan; murah dihapus sekarang, mahal diperbaiki nanti |
| 4 | **W8** kategori MCP/delegate sendiri | `c` saat ini bisa memberi izin lebih luas dari yang dipahami pengguna |
| 5 | **W5** persist mode | Guardrail yang reset diam-diam adalah guardrail yang salah |
| 6 | **W6, W9, W10** dokumentasi + tata letak | Murah, menghilangkan kebingungan |
| 7 | **W7** mode untuk subagent | Fitur, bukan perbaikan |

**Status:** belum ada yang diperbaiki. Audit ini murni temuan, plus satu test
pembuktian (`probe_w1_verify_bypass.rs`) yang sengaja **lulus** selama kebocoran
masih ada — ia berfungsi sebagai alarm yang berbunyi ketika W1 diperbaiki.

---

## Bagian 5 — Cara memverifikasi perbaikan nanti

Test yang harus ada setelah perbaikan:

1. `verify_with_ask_mode_requests_approval` — mode `Ask` + `--verify`, pastikan
   ada `Ask` dan bukan eksekusi diam-diam.
2. `verify_emits_the_actual_decision_not_always` — `ToolDecided` untuk verify
   mencerminkan keputusan sebenarnya.
3. `system_prompt_states_the_mode` — prompt untuk discuss memuat larangan mutasi,
   prompt execute tidak.
4. `switching_mode_reaches_the_next_turn_history` — setelah `/discuss`, pesan
   pertama turn berikutnya menyebut mode.
5. `mode_survives_resume` — `--resume` memulihkan mode tersimpan.
6. `other_category_is_not_covered_by_allow_category` — `c` pada `mcp_call` tidak
   mengizinkan `delegate_task`.

Setiap test harus dibuktikan **load-bearing** dengan mutasi balik, seperti yang
sudah dilakukan untuk penegakan discuss.
