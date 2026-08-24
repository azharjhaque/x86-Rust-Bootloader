#![no_main]
#![no_std]

use core::time::Duration;

use uefi::boot;
use uefi::prelude::*;

mod qemu_exit;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    log::info!("Rust_BL bootloader: milestone 1 toolchain smoke test");

    boot::stall(Duration::from_secs(2)); // so the log line is visible on screen

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}
