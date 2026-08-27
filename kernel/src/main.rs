#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use boot_info::{BootInfo, PixelFormatKind};

mod gdt;
mod idt;
mod interrupts;
mod port;
mod qemu_exit;
mod serial;

/// The kernel's entry point, reached by a `jmp` from the bootloader with
/// `BootInfo` in `rdi` and a stack the bootloader allocated.
///
/// # Safety
/// Called exactly once, by the bootloader, with a valid `BootInfo` pointer.
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    // Serial first: from here on every failure can announce itself,
    // including the validation immediately below.
    unsafe { serial::init() };
    kprintln!();
    kprintln!("=== Rust_BL kernel ===");

    if boot_info.is_null() {
        kprintln!("FATAL: boot_info pointer is null");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }
    let info = unsafe { &*boot_info };
    if !info.is_valid() {
        kprintln!("FATAL: boot_info failed validation (magic/version/format)");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    kernel_main(info)
}

/// Everything the kernel does, once `BootInfo` is known good.
fn kernel_main(info: &BootInfo) -> ! {
    kprintln!(
        "framebuffer: {}x{} stride={} @ {:#x}",
        info.framebuffer.width,
        info.framebuffer.height,
        info.framebuffer.stride,
        info.framebuffer.addr
    );
    kprintln!("kernel image: base={:#x} size={:#x}", info.kernel_base, info.kernel_size);

    init();
    selftest();

    let (skipped, fell_back) = fill_screen(info, 0x00, 0x33, 0x99);
    kprintln!("framebuffer painted");
    if fell_back {
        kprintln!("  note: pixel format was not recognised; assumed BGR");
    }
    if skipped > 0 {
        kprintln!("  note: {skipped} pixels skipped as out of bounds");
    }

    kprintln!("kernel initialised");
    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}

/// One-time CPU and device setup.
fn init() {
    unsafe { gdt::init() };
    kprintln!("GDT + TSS loaded (code selector {:#x})", gdt::KERNEL_CODE_SELECTOR);

    unsafe { idt::init() };
    kprintln!("IDT loaded");
}

/// Boot-time checks that the tables `init` installed actually work.
///
/// The breakpoint is a *trap*: the CPU resumes at the following
/// instruction, so reaching the line after it proves the handler both fired
/// and returned correctly through `iretq`.
fn selftest() {
    unsafe { core::arch::asm!("int3") };
    kprintln!("selftest: breakpoint handled and execution resumed");
}

/// Paint the whole framebuffer one colour — the simplest possible proof
/// that we reached the kernel and the framebuffer description is right.
///
/// Returns `(skipped_pixels, pixel_format_fell_back)`: the number of pixel
/// writes skipped because they would have landed past the framebuffer's
/// reported size, and whether `fb.pixel_format()` had to fall back to
/// `Bgr`. Both conditions are reportable now that the kernel has a serial
/// logger — see the call site in `kernel_main`.
fn fill_screen(info: &BootInfo, red: u8, green: u8, blue: u8) -> (u64, bool) {
    let fb = &info.framebuffer;

    // `info.is_valid()` already guarantees `fb.pixel_format()` is `Some`,
    // so this `unwrap_or` never actually falls back in practice. It is
    // written this way instead of `unwrap()` so a future change that
    // calls `fill_screen` without going through `is_valid()` first fails
    // safe (falls back to `Bgr`) rather than panicking with no logger and
    // no fault handler to report it.
    let pixel_format_fell_back = fb.pixel_format().is_none();
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
    let mut skipped_pixels: u64 = 0;
    for y in 0..fb.height as usize {
        for x in 0..fb.width as usize {
            let offset = (y * fb.stride as usize + x) * bytes_per_pixel;
            // Never write at or beyond the framebuffer's reported size.
            // This runs with no fault handler protecting it, so an
            // out-of-bounds write here would otherwise be invisible; count
            // it instead so the caller can report it.
            if offset + bytes_per_pixel > fb_size {
                skipped_pixels += 1;
                continue;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(pixel.as_ptr(), base.add(offset), bytes_per_pixel);
            }
        }
    }
    (skipped_pixels, pixel_format_fell_back)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // The formatting machinery allocates nothing, so this is safe even
    // here. If the UART itself is the problem this will hang in
    // `write_byte`, which is still more informative than a silent exit.
    kprintln!("KERNEL PANIC: {info}");
    qemu_exit::exit(qemu_exit::QemuExitCode::Failed)
}
