use core::arch::asm;

const ISA_DEBUG_EXIT_PORT: u16 = 0xf4;

// Note: a written value of 0 maps to process exit code 1 (see the doc
// comment on `exit` below), which is indistinguishable from QEMU's own
// generic error exit code. So 0 is deliberately not used as one of our
// marker values here.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Writes to the `isa-debug-exit` device, which shuts QEMU down with
/// process exit code `(value << 1) | 1`. Only meaningful when QEMU is
/// launched with `-device isa-debug-exit,iobase=0xf4,iosize=0x04` (which
/// `xtask` always does). On real hardware there is no such I/O port
/// listening, so the port write is silently ignored, but this function
/// still never returns: control falls through into the `hlt` loop below
/// and hangs there forever. This is fine since this project only targets
/// QEMU.
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
