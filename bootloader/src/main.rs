#![no_main]
#![no_std]

extern crate alloc;

use core::time::Duration;

use uefi::boot;
use uefi::cstr16;
use uefi::prelude::*;

mod elf;
mod file;
mod graphics;
mod memory;
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

    let loaded = match elf::load_kernel(&kernel_image) {
        Ok(loaded) => loaded,
        Err(error) => {
            log::error!("failed to load kernel ELF: {error:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    log::info!(
        "kernel loaded: entry={:#x} base={:#x} size={:#x}",
        loaded.entry,
        loaded.base,
        loaded.size
    );

    let framebuffer = match graphics::open_framebuffer() {
        Ok(info) => info,
        Err(status) => {
            log::error!("failed to open framebuffer: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    log::info!(
        "framebuffer: {}x{} stride={} addr={:#x} size={:#x} format={:?}",
        framebuffer.width,
        framebuffer.height,
        framebuffer.stride,
        framebuffer.addr,
        framebuffer.size,
        framebuffer.pixel_format
    );

    let (_regions_ptr, regions_capacity) =
        match memory::allocate_region_array(memory::REGION_CAPACITY) {
            Ok(pair) => pair,
            Err(status) => {
                log::error!("failed to allocate memory-region array: {status:?}");
                qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
            }
        };

    log::info!("memory-region array: capacity={regions_capacity}");

    boot::stall(Duration::from_secs(2)); // so the log line is visible on screen

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}
