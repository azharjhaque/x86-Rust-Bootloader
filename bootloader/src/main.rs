#![no_main]
#![no_std]

extern crate alloc;

use core::time::Duration;

use uefi::boot;
use uefi::cstr16;
use uefi::prelude::*;

mod file;
mod qemu_exit;

#[entry]
fn main() -> Status {
    if uefi::helpers::init().is_err() {
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    log::info!("Rust_BL bootloader: milestone 1 toolchain smoke test");

    let kernel_name = cstr16!("kernel.elf");
    let kernel_image = match file::read_file(kernel_name) {
        Ok(image) => image,
        Err(status) => {
            log::error!("failed to read kernel.elf: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    log::info!("loaded kernel.elf: {} bytes", kernel_image.len());

    boot::stall(Duration::from_secs(2)); // so the log line is visible on screen

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}
