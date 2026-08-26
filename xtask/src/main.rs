use std::env;
use std::fs;
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

/// How long to let QEMU run before assuming the bootloader hung and killing
/// it. Without this, a boot hang blocks `cargo xtask run` forever with no
/// way to report failure.
const QEMU_TIMEOUT: Duration = Duration::from_secs(60);

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => run(),
        _ => {
            eprintln!("Usage: cargo xtask run");
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

fn run() -> ExitCode {
    let root = workspace_root();

    if let Err(msg) = build_bootloader(&root) {
        eprintln!("FAIL: {msg}");
        return ExitCode::FAILURE;
    }

    if let Err(msg) = build_kernel(&root) {
        eprintln!("FAIL: {msg}");
        return ExitCode::FAILURE;
    }

    let esp_dir = match stage_esp(&root) {
        Ok(dir) => dir,
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let vars_copy = match copy_ovmf_vars(&root) {
        Ok(path) => path,
        Err(msg) => {
            eprintln!("FAIL: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let mut child = match Command::new("qemu-system-x86_64")
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"))
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars_copy.display()))
        .arg("-drive")
        .arg(format!("format=raw,file=fat:rw:{}", esp_dir.display()))
        .arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04")
        // Without this, a triple fault (reset) makes QEMU reboot the
        // firmware and try again instead of exiting. Milestone 3 starts
        // writing fault-handling code, where a bad interrupt/exception
        // path triple-faults rather than panicking cleanly, so this
        // matters starting now: with the default reboot-on-triple-fault
        // behavior, a fault silently loops until QEMU_TIMEOUT kills it 60
        // seconds later and reports a generic "boot hang", instead of
        // exiting immediately so the "expected 33, got N" branch below can
        // report it right away.
        .arg("-no-reboot")
        .arg("-m")
        .arg("256M")
        .arg("-display")
        .arg("none")
        .arg("-serial")
        .arg("stdio")
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!(
                "FAIL: failed to launch qemu-system-x86_64 (is it installed? try: sudo apt install qemu-system-x86): {e}"
            );
            return ExitCode::FAILURE;
        }
    };

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
            Err(e) => {
                eprintln!("FAIL: failed to poll qemu-system-x86_64: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    let status = match status {
        Some(status) => status,
        None => {
            // Boot hung: qemu never exited on its own within the deadline.
            // Kill it so the process doesn't linger, then report failure
            // instead of blocking forever.
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("FAIL: qemu timed out after 60s (boot hang?)");
            return ExitCode::FAILURE;
        }
    };

    match status.code() {
        Some(EXPECTED_EXIT_CODE) => {
            println!("PASS: bootloader exited with expected code {EXPECTED_EXIT_CODE}");
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
