use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS_SRC: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";
// Matches qemu_exit::QemuExitCode::Success (0x10): QEMU maps a written
// value `v` to process exit code `(v << 1) | 1`.
const EXPECTED_EXIT_CODE: i32 = 33;

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

fn run() -> ExitCode {
    build_bootloader();
    let esp_dir = stage_esp();
    let vars_copy = copy_ovmf_vars();

    let status = Command::new("qemu-system-x86_64")
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"))
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars_copy.display()))
        .arg("-drive")
        .arg(format!("format=raw,file=fat:rw:{}", esp_dir.display()))
        .arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04")
        .arg("-m")
        .arg("256M")
        .status()
        .expect(
            "failed to launch qemu-system-x86_64 (is it installed? try: sudo apt install qemu-system-x86)",
        );

    match status.code() {
        Some(EXPECTED_EXIT_CODE) => {
            println!("PASS: bootloader exited with expected code {EXPECTED_EXIT_CODE}");
            ExitCode::SUCCESS
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

fn build_bootloader() {
    let status = Command::new("cargo")
        .args(["build", "-p", "bootloader", "--target", "x86_64-unknown-uefi"])
        .status()
        .expect("failed to run cargo build");
    assert!(status.success(), "cargo build failed");
}

fn stage_esp() -> PathBuf {
    let esp_boot_dir = Path::new("target/esp/EFI/BOOT");
    fs::create_dir_all(esp_boot_dir).expect("failed to create ESP directory structure");
    fs::copy(
        "target/x86_64-unknown-uefi/debug/bootloader.efi",
        esp_boot_dir.join("BOOTX64.EFI"),
    )
    .expect("failed to copy bootloader.efi into the ESP (did the build produce it?)");
    Path::new("target/esp").to_path_buf()
}

fn copy_ovmf_vars() -> PathBuf {
    let dest = Path::new("target/OVMF_VARS.fd");
    fs::copy(OVMF_VARS_SRC, dest).expect(
        "failed to copy OVMF_VARS.fd (is the 'ovmf' package installed? try: sudo apt install ovmf)",
    );
    dest.to_path_buf()
}
