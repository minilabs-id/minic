//! Registry slash command TUI — satu sumber kebenaran.
//!
//! Sebelum ini nama command tersebar di tiga tempat (`run_command`, `push_help`,
//! dan pesan error `unknown command`), jadi menambah satu command berarti
//! menyunting tiga string dan mudah lupa. Modul ini menyatukannya, dan
//! `run_command` tetap menjadi tempat eksekusi (registry hanya metadata).
//!
//! Seluruh entri di sini dipakai dua hal: autocomplete mengambang di composer
//! dan teks bantuan. Karena itu ikon 🔒 pada `writes` sengaja ditampilkan di
//! keduanya — command yang menulis berkas harus terlihat berbeda dari yang
//! hanya membaca.

/// Apakah command memerlukan argumen tambahan?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// Tidak butuh argumen (mungkin menerima, tapi tidak berguna).
    None,
    /// Butuh argumen; setelah dipilih kursor tetap di composer.
    Required(&'static str),
}

/// Satu slash command yang dikenali TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    /// Nama tanpa garis miring, mis. `"model"`.
    pub name: &'static str,
    /// Alias yang juga dikenali (tanpa garis miring).
    pub aliases: &'static [&'static str],
    /// Keterangan singkat untuk daftar autocomplete dan `/help`.
    pub about: &'static str,
    pub args: ArgKind,
}

impl SlashCommand {
    /// Apakah command ini cocok dengan `typed` (tanpa garis miring)?
    pub fn matches(&self, typed: &str) -> bool {
        self.name.starts_with(typed) || self.aliases.iter().any(|a| a.starts_with(typed))
    }

    /// Teks yang dimasukkan ke composer saat entri dipilih.
    /// Command berargumen diakhiri spasi agar pengguna bisa langsung mengetik.
    pub fn insert_text(&self) -> String {
        match self.args {
            ArgKind::None => format!("/{}", self.name),
            ArgKind::Required(_) => format!("/{} ", self.name),
        }
    }
}

/// Semua command yang dikenal, berurutan seperti ditampilkan.
pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "model",
        aliases: &["models"],
        about: "pilih model dari provider (picker)",
        args: ArgKind::None,
    },
    SlashCommand {
        name: "connect",
        aliases: &["provider", "login"],
        about: "atur base URL + API key provider",
        args: ArgKind::None,
    },
    SlashCommand {
        name: "plan",
        aliases: &["plans"],
        about: "tinjau rencana tersimpan",
        args: ArgKind::None,
    },
    SlashCommand {
        name: "verify",
        aliases: &[],
        about: "jalankan perintah verifikasi sekarang",
        args: ArgKind::Required("CMD"),
    },
    SlashCommand {
        name: "help",
        aliases: &[],
        about: "tampilkan daftar command + tombol",
        args: ArgKind::None,
    },
    SlashCommand {
        name: "quit",
        aliases: &["exit"],
        about: "keluar dari minic",
        args: ArgKind::None,
    },
];

/// Masukan composer untuk autocomplete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CompletionState {
    /// Tidak ada daftar yang ditampilkan.
    #[default]
    Hidden,
    /// Daftar terbuka; `selected` adalah indeks ke hasil filter.
    Open { selected: usize },
}

/// Autocomplete mengambang untuk slash command.
///
/// Aturan tampil: composer hanya berisi satu baris, diawali `/`, dan belum ada
/// spasi. Begitu pengguna mengetik spasi (artinya masuk ke argumen), daftar
/// ditutup — kalau tidak, daftar akan menutupi transcript saat mengetik prompt
/// panjang yang kebetulan diawali garis miring.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    state: CompletionState,
    /// Hasil filter terakhir (indeks ke `COMMANDS`), agar `Enter`/`Tab` tidak
    /// perlu menghitung ulang dan tetap stabil bila input berubah.
    matches: Vec<usize>,
    /// Petunjuk argumen untuk command yang belum lengkap, mis. `CMD`.
    hint: Option<&'static str>,
}

impl Completion {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &CompletionState {
        &self.state
    }

    pub fn is_open(&self) -> bool {
        matches!(self.state, CompletionState::Open { .. })
    }

    pub fn selected_index(&self) -> usize {
        match self.state {
            CompletionState::Open { selected } => selected,
            CompletionState::Hidden => 0,
        }
    }

    /// Petunjuk argumen (mis. `CMD`) untuk command terpilih.
    pub fn hint(&self) -> Option<&'static str> {
        self.hint
    }

    /// Hasil filter saat ini, siap dirender.
    pub fn visible(&self) -> Vec<&'static SlashCommand> {
        self.matches.iter().map(|&i| &COMMANDS[i]).collect()
    }

    /// Segarkan daftar berdasarkan isi composer. `input` adalah teks mentah
    /// composer, `cursor` posisi kursor (untuk memastikan kursor di akhir).
    pub fn refresh(&mut self, input: &str, cursor: usize) {
        let typed = match parse_prefix(input, cursor) {
            Some(typed) => typed,
            None => {
                self.close();
                return;
            }
        };

        self.matches = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, cmd)| cmd.matches(typed))
            .map(|(i, _)| i)
            .collect();

        // Persis satu command dan sudah diketik lengkap: daftar tidak berguna
        // lagi, dan menutupinya membuat Enter terasa "menempel".
        let exact = self.matches.iter().any(|&i| COMMANDS[i].name == typed);
        if self.matches.is_empty() || (exact && self.matches.len() == 1) {
            self.close();
            return;
        }

        let selected = match self.state {
            CompletionState::Open { selected } if selected < self.matches.len() => selected,
            _ => 0,
        };
        self.state = CompletionState::Open { selected };
        self.hint = match COMMANDS[self.matches[selected]].args {
            ArgKind::Required(name) => Some(name),
            ArgKind::None => None,
        };
    }

    pub fn close(&mut self) {
        self.state = CompletionState::Hidden;
        self.matches.clear();
        self.hint = None;
    }

    /// Pindahkan pilihan. Daftar tidak pernah kosong saat terbuka.
    pub fn move_by(&mut self, delta: isize) {
        let CompletionState::Open { selected } = self.state else {
            return;
        };
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        let next = (selected as isize + delta).rem_euclid(len) as usize;
        self.state = CompletionState::Open { selected: next };
        self.hint = match COMMANDS[self.matches[next]].args {
            ArgKind::Required(name) => Some(name),
            ArgKind::None => None,
        };
    }

    /// Command yang sedang dipilih, bila daftar terbuka.
    pub fn selected_command(&self) -> Option<&'static SlashCommand> {
        let CompletionState::Open { selected } = self.state else {
            return None;
        };
        self.matches.get(selected).map(|&i| &COMMANDS[i])
    }

    /// Tinggi daftar: satu baris per nama, tanpa baris petunjuk.
    pub fn height(&self) -> usize {
        if !self.is_open() {
            return 0;
        }
        self.matches.len()
    }
}

/// Ambil awalan command bila composer layak menampilkan daftar.
///
/// Syarat: satu baris, kursor di akhir, diawali `/`, tanpa spasi setelahnya,
/// dan minimal satu huruf setelah garis miring.
fn parse_prefix(input: &str, cursor: usize) -> Option<&str> {
    if cursor != input.len() {
        return None;
    }
    if input.contains('\n') || input.contains('\r') {
        return None;
    }
    let typed = input.strip_prefix('/')?;
    if typed.is_empty() {
        // Composer baru berisi `/`: tampilkan seluruh daftar.
        return Some("");
    }
    if typed.contains(char::is_whitespace) {
        return None;
    }
    // Setelah command lengkap dan spasi, pengguna sedang mengetik argumen.
    if typed.chars().any(|c| c == '/') {
        return None;
    }
    Some(typed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refreshed(input: &str) -> Completion {
        let mut c = Completion::new();
        c.refresh(input, input.len());
        c
    }

    #[test]
    fn slash_alone_lists_every_command() {
        let c = refreshed("/");
        assert!(c.is_open());
        assert_eq!(c.visible().len(), COMMANDS.len());
    }

    #[test]
    fn typing_filters_by_prefix() {
        let c = refreshed("/mo");
        assert_eq!(c.visible().len(), 1);
        assert_eq!(c.visible()[0].name, "model");
    }

    #[test]
    fn aliases_are_searchable() {
        // `/models` adalah alias `model`.
        let c = refreshed("/models");
        assert!(c.visible().iter().any(|cmd| cmd.name == "model"));
    }

    #[test]
    fn exact_complete_command_closes_the_list() {
        let c = refreshed("/model");
        assert!(!c.is_open(), "command lengkap → daftar tutup");
    }

    #[test]
    fn space_closes_the_list_because_arguments_started() {
        let c = refreshed("/verify ");
        assert!(!c.is_open(), "spasi berarti masuk argumen");
    }

    #[test]
    fn ordinary_prompt_never_opens_the_list() {
        let c = refreshed("help me fix the build");
        assert!(!c.is_open());
    }

    #[test]
    fn multiline_input_does_not_open_the_list() {
        let input = "/mod\nrest";
        let mut c = Completion::new();
        c.refresh(input, input.len());
        assert!(!c.is_open());
    }

    #[test]
    fn cursor_not_at_end_keeps_list_closed() {
        let mut c = Completion::new();
        c.refresh("/mod", 2);
        assert!(
            !c.is_open(),
            "kursor di tengah bukan sedang memilih command"
        );
    }

    #[test]
    fn unknown_prefix_yields_no_matches() {
        let c = refreshed("/zzz");
        assert!(!c.is_open());
        assert!(c.visible().is_empty());
    }

    #[test]
    fn move_by_wraps_around() {
        let mut c = refreshed("/");
        assert_eq!(c.selected_index(), 0);
        c.move_by(-1);
        assert_eq!(c.selected_index(), COMMANDS.len() - 1);
        c.move_by(1);
        assert_eq!(c.selected_index(), 0);
    }

    #[test]
    fn selection_is_clamped_when_filter_shrinks() {
        // Pilih entri jauh di bawah, lalu filter menyempit drastis: indeks lama
        // akan melewati batas dan harus dijepit, bukan membuat panic/entri salah.
        let mut c = refreshed("/");
        c.move_by(5);
        assert_eq!(c.selected_index(), 5);
        c.refresh("/mod", 4);
        assert!(c.is_open(), "nama belum lengkap → daftar tetap terbuka");
        assert_eq!(c.visible().len(), 1);
        assert_eq!(c.selected_index(), 0, "pilihan dijepit ke entri yang ada");
        assert_eq!(c.selected_command().map(|cmd| cmd.name), Some("model"));
    }

    #[test]
    fn insert_text_adds_trailing_space_only_when_arguments_are_needed() {
        let model = COMMANDS.iter().find(|c| c.name == "model").unwrap();
        assert_eq!(model.insert_text(), "/model");
        let verify = COMMANDS.iter().find(|c| c.name == "verify").unwrap();
        assert_eq!(verify.insert_text(), "/verify ");
    }

    #[test]
    fn every_command_has_a_non_empty_description() {
        for cmd in COMMANDS {
            assert!(
                !cmd.about.trim().is_empty(),
                "{} tanpa keterangan",
                cmd.name
            );
        }
    }

    #[test]
    fn command_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for cmd in COMMANDS {
            assert!(seen.insert(cmd.name), "nama duplikat: {}", cmd.name);
            for alias in cmd.aliases {
                assert!(seen.insert(*alias), "alias duplikat: {alias}");
            }
        }
    }
}
