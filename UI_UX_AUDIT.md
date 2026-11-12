# Audit UI/UX — TUI minic

Catatan hasil audit. **Tidak ada kode yang diubah.** Semua temuan diverifikasi
dengan membaca langsung `crates/cli/src/tui/app.rs` (state machine, murni) dan
`crates/cli/src/tui/mod.rs` (driver ratatui). Test yang ada tetap hijau
(`cargo test -p minic --lib tui::` → 26 passed), jadi masalah di bawah ini
adalah **perilaku yang tidak tercakup test**, bukan regresi.

---

## 1. Ringkasan eksekutif

Akar masalah "scroll masih bermasalah" **bukan** tombolnya tidak terpasang.
PgUp/PgDn berfungsi. Masalahnya ada tiga lapis, dan yang paling terasa adalah
lapis pertama:

| # | Akar masalah | Dampak ke pengguna |
|---|---|---|
| **A** | Posisi scroll diukur **dari bawah** (`scroll_from_bottom`), bukan dari anchor tetap | Saat model sedang streaming, teks yang sedang Anda baca **terus meluncur naik dan kabur** dari layar. Rasanya seperti "scroll balik sendiri". |
| **B** | Transcript **hanya** bisa digulir dengan `PgUp`/`PgDn` | Mouse wheel dan panah ↑↓ tidak melakukan apa-apa. Di Windows Terminal, refleks pertama pengguna (wheel) mati total. |
| **C** | Status bar mencetak `End: bottom` padahal `End` **tidak** menggulir transcript | Hint itu bohong. Menekan `End` sesuai instruksi justru menggerakkan cursor composer. |

Kesimpulan singkat: *scroll bukan "tidak ada", tapi "tidak bisa dipegang"*.
Tidak ada satu pun mekanisme yang membuat posisi baca Anda **stabil** saat
konten bertambah.

---

## 2. Akar masalah A — posisi scroll meluncur saat streaming

### 2.1 Mekanismenya

State menyimpan jarak **dari ekor**, bukan posisi absolut:

```rust
// app.rs
/// Jarak dari bawah (0 = mengikuti ekor).
scroll_from_bottom: usize,   // :284
```

Dan setiap frame, driver menghitung ulang viewport dari state itu:

```rust
// app.rs:1237
pub fn viewport(&mut self, height: usize, width: usize) -> Viewport {
    let wrapped = self.wrapped(width);                            // :1238
    let max_offset = wrapped.len().saturating_sub(height.max(1));  // :1239
    self.scroll_from_bottom = self.scroll_from_bottom.min(max_offset);
    let offset = max_offset - self.scroll_from_bottom;            // :1241
    ...
}
```

Perhatikan `max_offset` **dihitung ulang setiap frame** dari jumlah baris
terbungkus. Selama turn berjalan, `AgentEvent::TextDelta` terus menambah
`assistant_buf`, sehingga `wrapped.len()` naik terus.

### 2.2 Kenapa itu merusak pengalaman baca

Redraw terjadi **setiap 100 ms** saat sibuk:

```rust
// mod.rs:190
let tick_ms = if app.busy() { 100 } else { 400 };
```

Mari ikuti satu skenario nyata:

1. Agent sedang jalan, transcript sudah 500 baris. Anda `PgUp` untuk membaca
   error yang muncul tadi → `scroll_from_bottom = 10`, `follow = false`
   (`app.rs:1198–1201`).
2. Anda mulai membaca. Satu detik kemudian model mengeluarkan 40 baris teks.
3. `wrapped.len()` naik 40 → `max_offset` naik 40.
4. `offset = max_offset - scroll_from_bottom` → **`offset` ikut naik 40**.
5. Baris yang sedang Anda baca bergeser 40 baris ke atas, keluar layar.

Artinya: **`scroll_from_bottom` konstan = konten yang dibaca ikut bergerak.**
Mode "berhenti mengikuti ekor" secara teknis aktif (`follow = false`, status
menampilkan `↑ N`), tapi secara visual Anda tetap "ditarik" oleh ekor. Ini
persis gejala yang dilaporkan.

Perbaikan konseptualnya: saat `follow == false`, posisi harus disimpan sebagai
**offset absolut dari atas** (`offset`), dan `offset` itu yang dipertahankan —
bukan dihitung ulang dari bawah. Konsekuensi turunan: `max_offset` boleh
berubah, tapi `offset` tidak boleh ikut bergerak.

### 2.3 Detail yang memperparah

- **`viewport()` adalah method `&mut self` yang dipanggil dari jalur render**
  (`mod.rs:650`, `draw_transcript`). Fungsi "hitung tampilan" punya efek samping
  menulis state (`:1240`). Ini yang membuat posisi scroll bergantung pada
  urutan/kapan frame digambar, bukan pada aksi pengguna.
- **Clamp di `:1240` memakan scroll Anda.** `scroll_from_bottom` di-*clamp* ke
  `max_offset`. Jika tinggi area transcript menyusut (composer tumbuh jadi 6
  baris saat Anda mengetik prompt panjang — `MAX_COMPOSER_ROWS = 6`,
  `app.rs:272`, `mod.rs:554–557`), `max_offset` mengecil dan posisi scroll Anda
  **dipotong permanen** tanpa pemberitahuan.
- **`begin_turn()` memaksa kembali ke ekor** (`app.rs:1087–1094`,
  `scroll_to_bottom()`). Jadi setiap submit, posisi baca hilang. Untuk alur
  "baca hasil → kirim prompt lanjutan" ini masuk akal, tapi digabung dengan A
  hasilnya: tidak pernah ada momen di mana scroll benar-benar "menempel".

---

## 3. Akar masalah B — hanya PgUp/PgDn, tanpa mouse dan tanpa panah

### 3.1 Mouse tidak pernah diaktifkan

```rust
// mod.rs:148
execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
```

Hanya *bracketed paste*. Tidak ada `EnableMouseCapture`, dan grep di seluruh
`crates/` untuk `MouseEvent` / `ScrollUp` / `ScrollDown` / `EnableMouseCapture`
**tidak menemukan apa pun**. Di Windows Terminal, scroll wheel menggerakkan
*scrollback terminal*, bukan transcript — dan karena TUI memakai alternate
screen, wheel tampak "tidak melakukan apa-apa".

### 3.2 Panah ↑↓ tidak pernah menggulir transcript

Urutan arm di `handle_key` (`app.rs:634–642`) menentukan segalanya:

```rust
Key::PageUp   => { self.scroll_up(10); ... }    // :634
Key::PageDown => { self.scroll_down(10); ... }  // :638
_ if self.busy => Action::None,                 // :642  ← gerbang
Key::Enter => { ... }
Key::Up   => { self.history_prev(); ... }       // :710
Key::Down => { self.history_next(); ... }       // :714
```

Akibatnya:

- saat **idle**, ↑↓ = navigasi history prompt (tidak pernah scroll);
- saat **busy**, ↑↓ = `Action::None` (tidak melakukan apa-apa sama sekali);
- **tidak ada kondisi di mana ↑↓ menggulir transcript.**

Catatan keadilan: README (`## Interactive TUI → Keys`, baris 368) memang
menulis `Up`/`Down` = "prompt history", jadi implementasi **konsisten dengan
dokumentasinya**. Yang
jadi masalah adalah UX, bukan pelanggaran kontrak: panah adalah refleks pertama
untuk menggulir, dan di sini panah justru mengubah isi composer secara tak
terduga saat pengguna sedang membaca. Perilaku ini juga **inkonsisten di dalam
aplikasi sendiri**: di panel tool output, ↑↓ *justru* menggulir
(`app.rs:516–523`), dan `j`/`k` juga (`:513`,`:519`). Jadi satu-satunya tempat
panah tidak bisa menggulir adalah transcript utama.

### 3.3 `PgUp` bukan "satu halaman"

```rust
Key::PageUp => { self.scroll_up(10); ... }    // app.rs:635
Key::PageDown => { self.scroll_down(10); ... } // app.rs:639
```

Jumlahnya **selalu 10 baris**, bukan tinggi viewport. Di terminal 50 baris,
`PgUp` menggeser kurang dari seperempat halaman; di terminal 15 baris, ia
melewati hampir satu halaman penuh. Nama tombolnya menjanjikan "page". Perilaku
ini konsisten dengan panel output (`:514`,`:520`) dan modal approval
(`:1129`,`:1133`), jadi ini keputusan desain yang diterapkan seragam — tapi
tetap membuat scroll transcript terasa lambat di layar besar.

### 3.4 `Home`/`End` juga tidak berlaku untuk transcript

```rust
Key::Home => { self.cursor = self.line_start(); ... }  // :718
Key::End  => { self.cursor = self.line_end();   ... }  // :722
```

Di konteks transcript, `Home`/`End` menggerakkan **cursor composer**. Di panel
output, `Home`/`End` menggulir (`app.rs:525–526`). Modal approval adalah arti
ketiga: ia mengabaikan `Home`/`End` sama sekali dan hanya menerima
`PgUp`/`PgDn` (`app.rs:1128–1136`). Empat arti berbeda untuk tombol navigasi
yang sama, tergantung layar mana yang sedang aktif.

### 3.5 Discoverability

Satu-satunya tempat yang menyebut PgUp adalah layar sambutan
(`mod.rs:663–694`, baris `Ctrl+J newline · ↑ history · PgUp scroll`) dan
`/help` (`app.rs:805–814`). Layar sambutan **hilang** begitu `is_empty()`
menjadi false (`mod.rs:646`). Setelah itu, pengguna baru tidak punya jalan
menemukan tombol scroll selain menebak.

---

## 4. Akar masalah C — hint status bar yang menyesatkan

```rust
// mod.rs:726
} else if !app.following() {
    "End: bottom".to_string()
}
```

Instruksinya jelas: tekan `End` untuk kembali ke bawah. Tapi `End` di konteks
transcript diarahkan ke cursor composer (`app.rs:722`), dan **tidak ada** kode
mana pun yang memanggil `scroll_to_bottom()` dari tombol `End`.

Konsekuensi: setelah pengguna `PgUp` sekali, status bar menampilkan `End: bottom`
**secara permanen** (karena `follow` hanya kembali true lewat `scroll_down` yang
mencapai 0, `app.rs:1203–1207`, atau lewat `scroll_to_bottom`). Satu-satunya cara
keluar adalah `PgDn` cukup banyak — yang tidak pernah disebut di mana pun. Ini
dead-end navigasi.

---

## 5. Temuan lain (di luar scroll)

### P1 — Panel tool output mati saat paling dibutuhkan

```rust
_ if self.busy => Action::None,   // app.rs:642
Key::Enter => { ... open_last_tool_output() ... }
```

`Enter` pada composer kosong membuka panel output tool — tapi gerbang
`_ if self.busy` muncul **sebelum** arm `Enter`. Jadi selama agent berjalan
(tepat saat Anda ingin mengintip hasil `bash` yang baru saja selesai), panel
tidak bisa dibuka. Perlu `Enter` dipindah ke atas gerbang, atau panel dibuka
dari jalur lain.

### P1 — Paste dibuang diam-diam saat sibuk

```rust
// app.rs:773
if self.busy || self.approval.is_some() {
    return;
}
```

`Ctrl+V`/paste saat streaming tidak menghasilkan feedback apa pun. Pengguna
mengira paste gagal/kepanasan. Sebaiknya minimal `push_note("paste ignored while running")`.

### P2 — Composer yang tumbuh menggeser transcript

`composer.height` berkisar 1–6 baris (`app.rs:272`, `mod.rs:554–557`), dan
`chunks[0]` (`Constraint::Min(1)`) menyusut karenanya. Setiap kali Anda menambah
baris baru di prompt, area transcript menyusut dan — karena A — posisi baca
bergeser. Layar terasa "berdenyut".

### P2 — `Tab` adalah tombol mati

```rust
Key::Tab => Action::None,   // app.rs:726
```

Di composer, `Tab` tidak melakukan apa pun. Padahal README
(`## Interactive TUI → Keys`) tidak mencantumkannya, jadi ini juga bukan
kontrak yang dilanggar — tapi ini slot keyboard yang terbuang (kandidat:
autocomplete slash command, atau scroll transcript).

### P2 — `Ctrl+C` / `Enter` di panel output tidak sesuai dokumentasi

Dokumentasi (`README` → "Reading a tool's output") menyebut penutup panel
adalah `Esc`/`q`. Kode juga menerima `Enter` dan `Ctrl+C`
(`app.rs:510–512`). `Ctrl+C` yang menutup panel (bukan cancel/quit) mudah
disalahpahami sebagai "turn dibatalkan".

### P2 — Tidak ada penanda batas turn

Transcript hanya menumpuk `Line::User` / `Line::Assistant` / `Line::Tool` dengan
awalan `❯` dan spasi (`app.rs:1299–1305`). Pada sesi panjang, sulit membedakan
"jawaban model yang mana untuk prompt yang mana", terutama setelah resume.
Tidak ada separator, nomor turn, atau timestamp.

### P2 — Status bar bisa terpotong habis

```rust
// mod.rs:701
let left_width = area.width.saturating_sub(30).max(10) as usize;
```

Kanan dikunci 30 kolom (`Constraint::Length(30)`, `:734`). Pada terminal sempit
(≤ 60 kolom), kiri hanya dapat 30 kolom, dan karena indikator `↑ N`/`↓ N`
**ditambahkan paling akhir** (`:716–721`), indikator itulah yang pertama
terpotong oleh `truncate` — tepat informasi yang paling dibutuhkan saat scroll.

### P2 — `viewport()` punya efek samping di jalur render

Sudah disebut di §2.3, tapi layak dicatat sebagai isu arsitektur: `draw`
mengambil `&mut App`. Merender tidak boleh mengubah state; ini yang membuat
"posisi scroll" menjadi fungsi dari kapan frame terakhir digambar.

---

## 6. Verifikasi

```
$ cargo test -p minic --lib tui::
running 26 tests
..........................
test result: ok. 26 passed; 0 failed
```

Test yang ada justru **mengunci** perilaku saat ini, dan perlu diperbarui bila
A–C diperbaiki:

- `app.rs:1782` `scroll_is_bottom_anchored_and_clamped` — mengunci model
  bottom-anchored.
- `crates/cli/tests/m5_app.rs:99` `scroll_clamps_and_ctrlc_quits` — mengunci
  `scroll()` = jarak dari bawah (PgUp ×2 = 20).
- `app.rs:2099` `output_panel_scrolls_and_closes` — mengunci `End` = scroll
  **di dalam panel** (bukan di transcript), yang mempertegas inkonsistensi §3.4.

Tidak ada test untuk: mouse scroll, panah ↑↓ di transcript, arti `End` di
konteks transcript, atau stabilitas posisi saat streaming (A). Itu sebabnya
masalah ini lolos.

---

## 7. Rekomendasi berurutan (bila nanti diperbaiki)

1. **Ubah model scroll jadi anchor absolut** (§2). Simpan `offset` dari atas;
   saat `follow == false`, `offset` tidak boleh berubah walau konten bertambah.
   Ini memperbaiki akar A dan sebagian besar keluhan.
2. **Aktifkan mouse capture + tangani `Event::Mouse`** (§3.1), minimal
   `ScrollUp`/`ScrollDown` → `scroll_up`/`scroll_down`.
3. **Petakan ↑↓ ke scroll transcript saat `!following()`**, dan biarkan tetap
   = history bila `following()` (§3.2). Ini pola yang dipakai kebanyakan TUI.
4. **Implementasikan `End` = `scroll_to_bottom()`** di konteks transcript, atau
   hapus hint `End: bottom` (§4). Jangan biarkan hint berbohong.
5. **Pindahkan `Enter` (panel output) ke atas gerbang `busy`** (§5-P1).
6. **Beri feedback saat paste ditolak** (§5-P1).
7. **Pastikan indikator `↑ N`/`↓ N` tidak terpotong** pada terminal sempit
   (§5-P2).
8. Pisahkan perhitungan viewport dari mutasi state (jadikan `&self`) supaya
   perilaku render bisa diuji murni.

---

## 8. Catatan positif (jangan ikut "diperbaiki")

Hal-hal berikut sudah benar dan layak dipertahankan:

- **Sanitasi seluruh teks tak tepercaya** sebelum tampil (`render/text.rs`),
  termasuk ANSI/OSC dan kontrol C0.
- **Wrap berbasis kolom** (`unicode-width`), CJK/emoji tidak merusak alignment —
  ada test-nya (`composer_wraps_by_display_width`).
- **Satu baris per tool call**, di-update di tempat (`tool_index`), tidak
  duplikat — ada test (`tool_line_updates_in_place`).
- **Fail-closed** di seluruh jalur approval, termasuk channel tertutup.
- **Ctrl+C saat busy = cancel, saat idle = quit** — semantik yang benar dan
  teruji.
- **Turn yang dibatalkan tidak bisa bocor** ke UI (nomor `seq` di `mod.rs`).
- **Event turn lama diabaikan** setelah `Ctrl+C` (`active_seq`).
