//! Raw x86 port I/O.
//!
//! The `in`/`out` instructions are the only way to reach legacy devices
//! like the UART and the PIC. There is no safe wrapper here on purpose:
//! writing to an arbitrary port can reconfigure hardware underneath the
//! running system, so every call site states why its port and value are
//! correct.

use core::arch::asm;

/// Write a byte to an I/O port.
///
/// # Safety
/// The caller must ensure `port` is a device that expects this write, and
/// that the write is valid for the device's current state.
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Read a byte from an I/O port.
///
/// # Safety
/// The caller must ensure `port` is a device that expects to be read, and
/// that reading it has no unwanted side effects (some device registers
/// clear-on-read).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
