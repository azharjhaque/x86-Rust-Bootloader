#![no_std]
#![no_main]

use boot_info::BootInfo;

/// The kernel's entry point.
///
/// The bootloader jumps here after `ExitBootServices` with a pointer to
/// [`BootInfo`] in `rdi` (the SysV first-argument register) and `rsp`
/// pointing at a stack the bootloader allocated for us.
///
/// # Safety
/// Called exactly once, by the bootloader, with a valid `BootInfo`
/// pointer. Never returns — there is nothing to return to.
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(_boot_info: *const BootInfo) -> ! {
    halt()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    halt()
}

/// Park the CPU. `hlt` in a loop rather than a bare spin so the core
/// idles instead of burning power; the loop guards against spurious
/// wakeups from interrupts.
fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) }
    }
}
