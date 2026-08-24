use core::arch::asm;

const ISA_DEBUG_EXIT_PORT: u16 = 0xf4;

#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Writes to the `isa-debug-exit` device, which shuts QEMU down with
/// process exit code `(value << 1) | 1`. Only meaningful when QEMU is
/// launched with `-device isa-debug-exit,iobase=0xf4,iosize=0x04` (which
/// `xtask` always does); this is a no-op on real hardware, which is fine
/// since this project only targets QEMU.
pub fn exit(code: QemuExitCode) -> ! {
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") ISA_DEBUG_EXIT_PORT,
            in("eax") code as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
    loop {
        unsafe { asm!("hlt", options(nomem, nostack)) }
    }
}
