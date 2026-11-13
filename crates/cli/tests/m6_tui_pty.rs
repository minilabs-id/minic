//! M6-f: test PTY nyata untuk TUI.
//!
//! Menjalankan binary `minic --tui --fake` di pseudo-terminal, lalu
//! memverifikasi:
//! 1. masuk alternate screen (`ESC[?1049h`),
//! 2. Ctrl+C keluar dan layar dipulihkan (`ESC[?1049l`).
//!
//! Bila PTY tidak bisa dibuat/di-spawn di mesin ini, test **di-skip** dengan
//! alasan yang jelas (bukan hijau palsu) — pola yang sama dengan baseline.

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_pty_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn contains(buffer: &Arc<Mutex<Vec<u8>>>, needle: &[u8]) -> bool {
    let data = buffer.lock().unwrap();
    data.windows(needle.len()).any(|window| window == needle)
}

fn wait_until(buffer: &Arc<Mutex<Vec<u8>>>, needle: &[u8], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if contains(buffer, needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    contains(buffer, needle)
}

#[test]
fn tui_enters_and_restores_alternate_screen() {
    let root = temp_root("tui");

    let pty_system = native_pty_system();
    let Ok(pair) = pty_system.openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        eprintln!("skip: openpty failed on this host");
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_minic"));
    command.args(["--tui", "--fake", "--cwd"]);
    command.arg(&root);
    command.cwd(&root);

    let Ok(mut child) = pair.slave.spawn_command(command) else {
        eprintln!("skip: spawn_command failed on this host");
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));

    let reader_buffer = buffer.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => reader_buffer.lock().unwrap().extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
    });

    let entered = wait_until(&buffer, b"\x1b[?1049h", Duration::from_secs(15));

    // Kirim satu turn nyata: teks + Enter. Provider fake akan memanggil
    // read_file lalu menjawab "Done." — membuktikan event loop, render, dan
    // penyelesaian turn bekerja di terminal sungguhan.
    let _ = writer.write_all(b"hi\r");
    let _ = writer.flush();
    let answered = wait_until(&buffer, b"Done.", Duration::from_secs(15));

    // Ctrl+C harus keluar dan memulihkan layar.
    let _ = writer.write_all(&[0x03]);
    let _ = writer.flush();
    let restored = wait_until(&buffer, b"\x1b[?1049l", Duration::from_secs(15));

    // Beri proses waktu keluar; bunuh bila masih hidup.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                break;
            }
        }
    }

    drop(writer);
    drop(pair.master);
    let _ = reader_thread.join();
    let _ = std::fs::remove_dir_all(&root);

    assert!(entered, "TUI harus masuk alternate screen");
    assert!(answered, "turn fake harus selesai dan menampilkan jawaban");
    assert!(restored, "TUI harus memulihkan layar setelah Ctrl+C");
}

/// Ketik `minic` polos di terminal harus membuka TUI.
///
/// Dulu pengguna harus tahu flag `--tui`; tanpa itu binary keluar dengan
/// "no prompt provided". Untuk tool yang dipasang lewat `winget` dan dipanggil
/// sebagai `minic`, perilaku itu salah: perintah tanpa argumen adalah cara
/// paling wajar memakainya.
#[test]
fn bare_invocation_opens_the_tui() {
    let root = temp_root("bare");

    let pty_system = native_pty_system();
    let Ok(pair) = pty_system.openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        eprintln!("skip: openpty failed on this host");
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    // Tanpa `--tui`: hanya `--fake` (agar tidak butuh provider) dan `--cwd`.
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_minic"));
    command.args(["--fake", "--cwd"]);
    command.arg(&root);
    command.cwd(&root);

    let Ok(mut child) = pair.slave.spawn_command(command) else {
        eprintln!("skip: spawn_command failed on this host");
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));

    let reader_buffer = buffer.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => reader_buffer.lock().unwrap().extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
    });

    let entered = wait_until(&buffer, b"\x1b[?1049h", Duration::from_secs(15));

    // Harus benar-benar TUI, bukan pesan error "no prompt provided".
    let data = buffer.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&data);
    assert!(
        !text.contains("no prompt provided"),
        "`minic` polos harus membuka TUI, bukan meminta prompt: {text}"
    );

    let _ = writer.write_all(&[0x03]);
    let _ = writer.flush();
    let _ = wait_until(&buffer, b"\x1b[?1049l", Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                break;
            }
        }
    }
    drop(reader_thread);
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        entered,
        "`minic` tanpa argumen harus masuk alternate screen"
    );
}
