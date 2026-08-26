#![no_main]
#![no_std]

extern crate alloc;

use boot_info::BootInfo;
use uefi::boot;
use uefi::cstr16;
use uefi::prelude::*;

mod elf;
mod file;
mod graphics;
mod handoff;
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

    // The image is fully copied into the kernel's own pages now, and main()
    // diverges so this Vec's destructor would never run. Free it while boot
    // services still can, instead of leaking ~768 KB into the memory map.
    drop(kernel_image);

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

    let (regions_ptr, regions_capacity) =
        match memory::allocate_region_array(memory::REGION_CAPACITY) {
            Ok(pair) => pair,
            Err(status) => {
                log::error!("failed to allocate memory-region array: {status:?}");
                qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
            }
        };

    log::info!("memory-region array: capacity={regions_capacity}");

    let stack_top = match handoff::allocate_kernel_stack() {
        Ok(top) => top,
        Err(status) => {
            log::error!("failed to allocate kernel stack: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    // BootInfo itself must outlive boot services, so it goes in
    // LOADER_DATA rather than on our soon-to-be-invalid stack.
    let boot_info_ptr = match boot::allocate_pages(
        uefi::boot::AllocateType::AnyPages,
        uefi::mem::memory_map::MemoryType::LOADER_DATA,
        1,
    ) {
        Ok(ptr) => ptr.as_ptr().cast::<BootInfo>(),
        Err(status) => {
            log::error!("failed to allocate BootInfo: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    unsafe {
        boot_info_ptr.write(BootInfo::new(
            framebuffer,
            regions_ptr as u64,
            0, // filled in after ExitBootServices, when the map is final
            loaded.base,
            loaded.size,
        ));
    }

    log::info!("handing off to kernel at {:#x}", loaded.entry);

    unsafe {
        handoff::exit_and_jump(
            boot_info_ptr,
            regions_ptr,
            regions_capacity,
            loaded.entry,
            stack_top,
        )
    }
}
