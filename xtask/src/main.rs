use std::env;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS_SRC: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";
// Matches qemu_exit::QemuExitCode::Success (0x10): QEMU maps a written
// value `v` to process exit code `(v << 1) | 1`. Note a written value of 0
// would map to process exit code 1, indistinguishable from QEMU's own
// generic error exit, so 0 is deliberately never used as one of these
// marker codes.
const EXPECTED_EXIT_CODE: i32 = 33;
// Matches qemu_exit::QemuExitCode::Failed (0x11): (0x11 << 1) | 1 = 35.
const FAILURE_EXIT_CODE: i32 = 35;

/// Where this run's guest framebuffer capture is written.
fn screenshot_path() -> PathBuf {
    PathBuf::from(format!("/tmp/rust_bl_screen_{}.ppm", std::process::id()))
}
/// The GOP surface dimensions QEMU exposes to this test.
const KERNEL_SCREENSHOT_DIMENSIONS: &[u8] = b"1280 800";

/// How long to let QEMU run before assuming the bootloader hung and killing
/// it. Without this, a boot hang blocks `cargo xtask run` forever with no
/// way to report failure.
const QEMU_TIMEOUT: Duration = Duration::from_secs(60);

/// Where QEMU's monitor socket lives. `xtask` connects to this to inject
/// keystrokes, which is how the keyboard IRQ gets tested without a human.
///
/// Includes this process's pid so two concurrent `cargo xtask` runs don't
/// collide on the same path: without that, the second run's
/// `fs::remove_file` (whose error is deliberately ignored, since "the file
/// doesn't exist yet" is the common case) could remove the *first* run's
/// live socket, or the second QEMU could simply fail to bind a path still
/// held by the first — either way degrading silently into a confusing
/// keyboard-injection failure instead of a clear error.
fn monitor_socket_path() -> String {
    format!("/tmp/rust_bl_monitor_{}.sock", std::process::id())
}

const USAGE: &str = "Usage:
  cargo xtask run     Build, stage the ESP, and boot in QEMU (expects exit 33)
  cargo xtask test    Boot a deliberately corrupted kernel image and check
                      that the bootloader rejects it (expects exit 35)";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => run(),
        Some("test") => test(),
        Some(other) => {
            eprintln!("unknown command `{other}`\n\n{USAGE}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root, derived from xtask's own manifest directory so that
/// `cargo xtask run` behaves the same regardless of the shell's current
/// working directory (e.g. running it from `bootloader/` instead of the
/// workspace root).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask's Cargo.toml should have a parent directory (the workspace root)")
        .to_path_buf()
}

/// Boot the current ESP and check that the kernel reached its success path.
fn run() -> ExitCode {
    let root = workspace_root();

    let staged = match build_and_stage(&root) {
        Ok(staged) => staged,
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let screenshot = screenshot_path();
    let observed = match boot_qemu(&staged, true, &screenshot) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            return ExitCode::FAILURE;
        }
    };

    match observed {
        Some(EXPECTED_EXIT_CODE) => {
            if let Err(msg) = check_screen_has_text(&screenshot) {
                eprintln!("FAIL: {msg}");
                return ExitCode::FAILURE;
            }
            println!("PASS: bootloader exited with expected code {EXPECTED_EXIT_CODE}, and the screen has text");
            ExitCode::SUCCESS
        }
        Some(FAILURE_EXIT_CODE) => {
            eprintln!("FAIL: bootloader reported an internal failure (exit {FAILURE_EXIT_CODE})");
            ExitCode::FAILURE
        }
        Some(other) => {
            eprintln!("FAIL: expected exit code {EXPECTED_EXIT_CODE}, got {other}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("FAIL: qemu-system-x86_64 exited via signal, no exit code");
            ExitCode::FAILURE
        }
    }
}

/// Check that the guest actually drew something other than the background.
///
/// Before this milestone the captured framebuffer contained exactly one
/// colour, because a solid fill was the only thing ever drawn. More than one
/// means glyphs reached the screen. This is deliberately a weak assertion
/// about *what* was drawn and a strong one about *whether* anything was --
/// the serial trace already covers content.
fn check_screen_has_text(screenshot: &Path) -> Result<(), String> {
    let data = fs::read(screenshot)
        .map_err(|e| format!("no screen capture at {}: {e}", screenshot.display()))?;
    // PPM: "P6\n<w> <h>\n<maxval>\n" then raw RGB triples.
    let mut parts = data.splitn(4, |b| *b == b'\n');
    let magic = parts.next().unwrap_or(b"");
    if magic != b"P6" {
        return Err("screen capture is not a P6 PPM".to_string());
    }
    let dimensions = parts.next().unwrap_or(b"");
    if dimensions != KERNEL_SCREENSHOT_DIMENSIONS {
        return Err(format!(
            "screen capture is not the 1280x800 kernel surface (got {})",
            String::from_utf8_lossy(dimensions)
        ));
    }
    parts.next();
    let pixels = parts.next().unwrap_or(b"");

    let mut first: Option<[u8; 3]> = None;
    for chunk in pixels.chunks_exact(3) {
        let rgb = [chunk[0], chunk[1], chunk[2]];
        match first {
            None => first = Some(rgb),
            Some(seen) if seen != rgb => return Ok(()),
            _ => {}
        }
    }
    Err("the screen shows a single flat colour \u{2014} no text was drawn".to_string())
}

/// Negative test: corrupt the staged kernel's ELF magic and check that the
/// bootloader rejects it *before* `ExitBootServices` rather than jumping
/// into a bad image.
///
/// This deliberately boots QEMU against an already-staged ESP instead of
/// going through the normal build path: `build_and_stage` rewrites
/// `kernel.elf` from the build output every time, which would overwrite the
/// corruption before QEMU ever read it.
///
/// Why this test exists: `run` only ever exercises the happy path. Every
/// validation branch in the ELF loader — magic, class, endianness, machine,
/// bounds and overflow checks, entry-point range — is dead code on a valid
/// kernel. Without a negative test, all of them could be deleted and `run`
/// would still pass. The distinction matters here because the loader
/// validates *before* the point of no return: caught early it is a logged
/// error and a clean exit, missed it is a triple fault with no logger left
/// to report anything.
fn test() -> ExitCode {
    let root = workspace_root();

    let staged = match build_and_stage(&root) {
        Ok(staged) => staged,
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let kernel = root.join("target/esp/kernel.elf");
    let pristine = match fs::read(&kernel) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("FAIL: failed to read staged kernel.elf: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut corrupted = pristine.clone();
    if corrupted.len() < 4 {
        eprintln!("FAIL: staged kernel.elf is too small to corrupt");
        return ExitCode::FAILURE;
    }
    // Replace the 4-byte ELF magic (0x7F "ELF") so the loader's very first
    // check is the one that trips.
    corrupted[..4].copy_from_slice(b"XXXX");

    if let Err(e) = fs::write(&kernel, &corrupted) {
        eprintln!("FAIL: failed to write corrupted kernel.elf: {e}");
        return ExitCode::FAILURE;
    }

    println!("corrupted the staged kernel's ELF magic; booting...");
    // No keystroke injection here: this run is expected to fail before the
    // kernel ever enables interrupts, so there is nothing for it to prove
    // and no benefit to spawning the injector thread.
    let observed = boot_qemu(&staged, false, &screenshot_path());

    // Restore before interpreting the result, so a failed boot never leaves
    // a corrupted image staged for the next `cargo xtask run`.
    if let Err(e) = fs::write(&kernel, &pristine) {
        eprintln!("FAIL: could not restore kernel.elf: {e}");
        eprintln!("      re-run `cargo xtask run` to re-stage a good image.");
        return ExitCode::FAILURE;
    }
    println!("restored the original kernel.elf");

    let observed = match observed {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            return ExitCode::FAILURE;
        }
    };

    match observed {
        Some(FAILURE_EXIT_CODE) => {
            println!(
                "PASS: bootloader rejected the corrupted image (exit {FAILURE_EXIT_CODE}) \
                 instead of jumping into it"
            );
            ExitCode::SUCCESS
        }
        Some(EXPECTED_EXIT_CODE) => {
            eprintln!(
                "FAIL: got the success code {EXPECTED_EXIT_CODE} from a corrupted image — \
                 the loader's validation did not run"
            );
            ExitCode::FAILURE
        }
        Some(other) => {
            eprintln!("FAIL: expected exit code {FAILURE_EXIT_CODE}, got {other}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("FAIL: qemu-system-x86_64 exited via signal, no exit code");
            ExitCode::FAILURE
        }
    }
}

/// Everything QEMU needs to boot: the ESP directory to expose as a FAT
/// volume, and this run's writable copy of the OVMF variable store.
struct Staged {
    esp_dir: PathBuf,
    vars_copy: PathBuf,
}

/// Build both binaries and stage them into the ESP.
fn build_and_stage(root: &Path) -> Result<Staged, String> {
    build_bootloader(root)?;
    build_kernel(root)?;
    let esp_dir = stage_esp(root)?;
    let vars_copy = copy_ovmf_vars(root)?;
    Ok(Staged { esp_dir, vars_copy })
}

/// Type into the guest through QEMU's monitor.
///
/// Spawned on its own thread because QEMU is running concurrently: the
/// keystrokes have to arrive *after* the kernel has enabled interrupts, and
/// there is no signal for that other than time. Sending repeatedly is
/// simpler and more robust than trying to synchronise, and the kernel only
/// needs one to arrive.
///
/// # Escape hatch
/// Setting `XTASK_NO_KEYS` skips injection entirely. This exists so the
/// keyboard path has a negative test: without it, the only way to prove
/// injected keystrokes (as opposed to something else) are what the kernel
/// sees is to manually edit the kernel to stop counting them and revert it
/// afterward — which will not survive later milestones touching this code.
/// With it, `XTASK_NO_KEYS=1 cargo xtask run` should fail on the kernel's
/// own keyboard deadline instead.
fn inject_keystrokes(socket_path: String, screenshot: &Path) -> Result<(), String> {
    // Escape hatch for the negative test: with injection suppressed, the
    // kernel's own keyboard deadline should fire and the run should FAIL.
    // Without this, "the keyboard works" can only be disproved by editing
    // the kernel and reverting it.
    if std::env::var_os("XTASK_NO_KEYS").is_some() {
        eprintln!("note: XTASK_NO_KEYS set — not injecting keystrokes");
        return Ok(());
    }

    // Wait for QEMU to create the socket.
    let mut stream = None;
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(100));
        if let Ok(s) = UnixStream::connect(&socket_path) {
            stream = Some(s);
            break;
        }
    }
    let Some(mut stream) = stream else {
        return Err("could not reach the QEMU monitor for screen capture".to_string());
    };

    thread::sleep(Duration::from_millis(4_500));
    match fs::remove_file(screenshot) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("failed to remove old screen capture {}: {e}", screenshot.display())),
    }
    stream.write_all(format!("screendump {}\n", screenshot.display()).as_bytes())
        .map_err(|e| format!("failed to request screen capture: {e}"))?;
    thread::sleep(Duration::from_millis(500));
    fs::metadata(screenshot)
        .map_err(|e| format!("screen capture was not created at {}: {e}", screenshot.display()))?;

    thread::spawn(move || {
        // Keep injecting until the write fails (QEMU exited), which is
        // already the normal way this loop ends — not for a fixed number
        // of sends. The schedule has to outlast the kernel's *deadline*,
        // not just its timer wait: the kernel only starts counting
        // keystrokes from `sti` onward (anything earlier is dropped by
        // `ps2::init`'s drain) and reports failure roughly 11s after that
        // (1s of timer ticks, then a 10s keyboard wait). On a loaded or
        // slow host, OVMF boot + the bootloader + kernel init can itself
        // eat several seconds *before* `sti` ever runs, so a short,
        // time-bounded injection schedule can finish sending before the
        // kernel is even listening — every injected key lands too early,
        // gets silently dropped, and the run fails with "no input within
        // 10s" even though nothing is actually broken. The `0..40` bound
        // (20s) is purely a backstop against a wedged QEMU that never
        // exits on its own; in a healthy run this loop always ends via the
        // write failing, well before the bound is reached.
        for _ in 0..40 {
            thread::sleep(Duration::from_millis(500));
            if stream.write_all(b"sendkey a\n").is_err() {
                // QEMU exited — the run is over, which is the normal way
                // this loop ends.
                return;
            }
        }
    });
    Ok(())
}

/// Launch QEMU against the staged ESP and wait for it to exit.
///
/// `inject` controls whether the keystroke-injection thread is started at
/// all: `run` needs it to prove the keyboard IRQ works, but `test` boots a
/// deliberately-corrupted image that is expected to fail before the kernel
/// ever enables interrupts, so spawning an injector for it would be pure
/// overhead with nothing to prove.
///
/// Returns the process exit code, or `None` if QEMU was terminated by a
/// signal. Interpreting that code is the caller's job — `run` and `test`
/// expect different ones.
fn boot_qemu(staged: &Staged, inject: bool, screenshot: &Path) -> Result<Option<i32>, String> {
    let socket_path = monitor_socket_path();
    let _ = fs::remove_file(&socket_path);

    let mut child = Command::new("qemu-system-x86_64")
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,file={}",
            staged.vars_copy.display()
        ))
        .arg("-drive")
        .arg(format!(
            "format=raw,file=fat:rw:{}",
            staged.esp_dir.display()
        ))
        .arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04")
        // Without this, a triple fault (reset) makes QEMU reboot the
        // firmware and try again instead of exiting. Milestone 3 starts
        // writing fault-handling code, where a bad interrupt/exception
        // path triple-faults rather than panicking cleanly, so this
        // matters starting now: with the default reboot-on-triple-fault
        // behavior, a fault silently loops until QEMU_TIMEOUT kills it 60
        // seconds later and reports a generic "boot hang", instead of
        // exiting immediately so the "expected 33, got N" branch can
        // report it right away.
        .arg("-no-reboot")
        .arg("-m")
        .arg("256M")
        .arg("-display")
        .arg("none")
        .arg("-serial")
        .arg("stdio")
        .arg("-monitor")
        .arg(format!("unix:{socket_path},server,nowait"))
        .spawn()
        .map_err(|e| {
            format!(
                "failed to launch qemu-system-x86_64 (is it installed? try: sudo apt install qemu-system-x86): {e}"
            )
        })?;

    if inject {
    if inject {
        if let Err(msg) = inject_keystrokes(socket_path, screenshot) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(msg);
        }
    }
    }

    let deadline = Instant::now() + QEMU_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    break None;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("failed to poll qemu-system-x86_64: {e}")),
        }
    };

    match status {
        Some(status) => Ok(status.code()),
        None => {
            // Boot hung: qemu never exited on its own within the deadline.
            // Kill it so the process doesn't linger, then report failure
            // instead of blocking forever.
            let _ = child.kill();
            let _ = child.wait();
            Err(format!(
                "qemu timed out after {}s (boot hang?)",
                QEMU_TIMEOUT.as_secs()
            ))
        }
    }
}

fn build_bootloader(root: &Path) -> Result<(), String> {
    // Use the same cargo that launched xtask (respects rustup toolchain
    // overrides, custom PATH setups, etc.) rather than assuming a bare
    // "cargo" resolves to the right one.
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "bootloader", "--target", "x86_64-unknown-uefi"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("failed to run cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".to_string());
    }
    Ok(())
}

fn build_kernel(root: &Path) -> Result<(), String> {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "kernel", "--target", "x86_64-unknown-none"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("failed to run cargo build for kernel: {e}"))?;
    if !status.success() {
        return Err("cargo build failed for kernel".to_string());
    }
    Ok(())
}

fn stage_esp(root: &Path) -> Result<PathBuf, String> {
    let esp_boot_dir = root.join("target/esp/EFI/BOOT");
    fs::create_dir_all(&esp_boot_dir)
        .map_err(|e| format!("failed to create ESP directory structure: {e}"))?;
    fs::copy(
        root.join("target/x86_64-unknown-uefi/debug/bootloader.efi"),
        esp_boot_dir.join("BOOTX64.EFI"),
    )
    .map_err(|e| {
        format!("failed to copy bootloader.efi into the ESP (did the build produce it?): {e}")
    })?;
    fs::copy(
        root.join("target/x86_64-unknown-none/debug/kernel"),
        root.join("target/esp/kernel.elf"),
    )
    .map_err(|e| format!("failed to copy kernel into the ESP (did the build produce it?): {e}"))?;
    Ok(root.join("target/esp"))
}

fn copy_ovmf_vars(root: &Path) -> Result<PathBuf, String> {
    let dest = root.join("target/OVMF_VARS.fd");
    fs::copy(OVMF_VARS_SRC, &dest).map_err(|e| {
        format!(
            "failed to copy OVMF_VARS.fd (is the 'ovmf' package installed? try: sudo apt install ovmf): {e}"
        )
    })?;
    Ok(dest)
}
