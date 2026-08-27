#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use boot_info::{BootInfo, PixelFormatKind};

mod console;
mod font;
mod gdt;
mod idt;
mod interrupts;
mod keyboard;
mod pic;
mod pit;
mod port;
mod ps2;
mod qemu_exit;
mod serial;

/// Rate the PIT is programmed to interrupt at.
const TIMER_HZ: u32 = 100;

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

    // SAFETY: this is the one and only call, made here in `kernel_main`
    // before anything enables interrupts — see `init`'s own `# Safety`
    // section for why interrupts are already off at this point.
    unsafe { init() };
    selftest();

    let (skipped, fell_back) = fill_screen(info, 0x00, 0x33, 0x99);
    // SAFETY: called once, and `info` was validated in `_start`.
    unsafe { console::init(&info.framebuffer) };
    kprintln!("framebuffer painted");
    if fell_back {
        kprintln!("  note: pixel format was not recognised; assumed BGR");
    }
    if skipped > 0 {
        kprintln!("  note: {skipped} pixels skipped as out of bounds");
    }

    kprintln!("enabling interrupts");
    unsafe { interrupts::enable() };

    // Wait for the timer to prove itself. If IRQ0 never arrives this loop
    // never exits, and xtask's 60-second timeout reports the hang — which
    // is the correct outcome, with the serial trace showing how far we got.
    const TICKS_REQUIRED: u64 = 100;
    while idt::ticks() < TICKS_REQUIRED {
        interrupts::hlt();
    }
    kprintln!("timer: {TICKS_REQUIRED} ticks received — IRQ0 works");

    // Now wait for a keystroke. `xtask` injects one through QEMU's monitor;
    // a human running QEMU directly can just type. Bound the wait in ticks
    // so a dead IRQ1 reports itself rather than hanging until the harness
    // timeout — the timer is known good by this point, so it makes a
    // serviceable clock.
    kprintln!("waiting for a keypress...");
    let deadline = idt::ticks() + TIMER_HZ as u64 * 10;
    while idt::key_events_seen() == 0 {
        if idt::ticks() > deadline {
            kprintln!("keyboard: no input within 10s — IRQ1 is not delivering");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
        interrupts::hlt();
    }
    kprintln!("keyboard: {} key event(s) received — IRQ1 works", idt::key_events_seen());

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}

/// One-time CPU and device setup: installs the GDT/TSS, the IDT, remaps
/// the PICs, programs the PIT, and brings up the 8042 controller.
///
/// # Safety
/// Must be called exactly once, from `kernel_main`, before interrupts are
/// enabled. Every function this calls documents "call once, with
/// interrupts disabled" as its own precondition; nothing here enforces the
/// "once" half, and the "interrupts disabled" half holds only because it
/// is true on entry to `kernel_main` — which itself is true only because
/// `bootloader/src/handoff.rs` executes `cli` before jumping into the
/// kernel. That `cli`, in a different binary entirely, is the actual
/// load-bearing precondition for this whole function; nothing on this side
/// of the jump can check it, so it is recorded here instead.
unsafe fn init() {
    unsafe { gdt::init() };
    kprintln!("GDT + TSS loaded (code selector {:#x})", gdt::KERNEL_CODE_SELECTOR);

    unsafe { idt::init() };
    kprintln!("IDT loaded");

    unsafe { pic::init() };
    kprintln!("PICs remapped to vectors {}-{}", pic::PIC1_OFFSET, pic::PIC2_OFFSET + 7);

    unsafe { pit::init(TIMER_HZ) };
    kprintln!("PIT programmed at {TIMER_HZ} Hz");

    unsafe { ps2::init() };
    kprintln!("8042 PS/2 controller initialised");
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
