#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use boot_info::{BootInfo, PixelFormatKind};

mod gdt;
mod idt;
mod port;
mod qemu_exit;
mod serial;

#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    // Initialise serial first: from here on every failure can announce
    // itself, including the validation failures immediately below.
    unsafe { serial::init() };
    kprintln!();
    kprintln!("=== Rust_BL kernel ===");

    // The pointer comes from another binary, so check the contract before
    // trusting anything behind it.
    if boot_info.is_null() {
        kprintln!("FATAL: boot_info pointer is null");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    let info = unsafe { &*boot_info };
    if !info.is_valid() {
        kprintln!("FATAL: boot_info failed validation (magic/version/format)");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    kprintln!(
        "framebuffer: {}x{} stride={} @ {:#x}",
        info.framebuffer.width,
        info.framebuffer.height,
        info.framebuffer.stride,
        info.framebuffer.addr
    );
    kprintln!("kernel image: base={:#x} size={:#x}", info.kernel_base, info.kernel_size);

    unsafe { gdt::init() };
    kprintln!("GDT + TSS loaded (code selector {:#x})", gdt::KERNEL_CODE_SELECTOR);
    kprintln!("double-fault IST index: {}", gdt::DOUBLE_FAULT_IST_INDEX);

    unsafe { idt::init() };
    kprintln!("IDT loaded");

    // Raise a breakpoint exception on purpose. It is a trap, so the CPU
    // resumes at the following instruction — if the next line prints, the
    // handler ran and returned correctly.
    unsafe { core::arch::asm!("int3", options(nomem, nostack)) };
    kprintln!("resumed after breakpoint");

    fill_screen(info, 0x00, 0x33, 0x99);
    kprintln!("framebuffer painted");
    kprintln!("kernel reached the end of milestone 3 setup");

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}

/// Paint the whole framebuffer one colour — the simplest possible proof
/// that we reached the kernel and the framebuffer description is right.
fn fill_screen(info: &BootInfo, red: u8, green: u8, blue: u8) {
    let fb = &info.framebuffer;

    // `info.is_valid()` already guarantees `fb.pixel_format()` is `Some`,
    // so this `unwrap_or` never actually falls back in practice. It is
    // written this way instead of `unwrap()` so a future change that
    // calls `fill_screen` without going through `is_valid()` first fails
    // safe (falls back to `Bgr`) rather than panicking with no logger and
    // no fault handler to report it.
    let pixel_format = fb.pixel_format().unwrap_or(PixelFormatKind::Bgr);
    let pixel = match pixel_format {
        PixelFormatKind::Rgb => [red, green, blue, 0],
        PixelFormatKind::Bgr => [blue, green, red, 0],
    };

    // Both pixel formats this crate understands are 32 bits per pixel, so
    // `pixel` (4 bytes) always covers a whole pixel. Clamp the copy length
    // to the buffer's own size so a corrupted `bytes_per_pixel` cannot
    // turn this into an out-of-bounds read of `pixel` itself.
    let bytes_per_pixel = (fb.bytes_per_pixel as usize).min(pixel.len());

    let base = fb.addr as *mut u8;
    let fb_size = fb.size as usize;
    for y in 0..fb.height as usize {
        for x in 0..fb.width as usize {
            let offset = (y * fb.stride as usize + x) * bytes_per_pixel;
            // Never write at or beyond the framebuffer's reported size.
            // This runs with no fault handler and no logger, so an
            // out-of-bounds write here would otherwise be invisible.
            if offset + bytes_per_pixel > fb_size {
                continue;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(pixel.as_ptr(), base.add(offset), bytes_per_pixel);
            }
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // The formatting machinery allocates nothing, so this is safe even
    // here. If the UART itself is the problem this will hang in
    // `write_byte`, which is still more informative than a silent exit.
    kprintln!("KERNEL PANIC: {info}");
    qemu_exit::exit(qemu_exit::QemuExitCode::Failed)
}
