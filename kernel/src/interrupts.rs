//! Controlling the interrupt flag.
//!
//! This kernel is single-core with no threads, so the only thing that can
//! interrupt a sequence of instructions is an interrupt. Disabling them is
//! therefore a complete critical section — no lock is needed or would help.

use core::arch::asm;

/// Run `f` with interrupts disabled, restoring the previous state after.
///
/// Restoring the *previous* state rather than unconditionally enabling
/// matters: this is called from inside interrupt handlers, where interrupts
/// are already off, and blindly running `sti` there would allow reentrancy
/// the handler is not written for.
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let flags: u64;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) };

    // Bit 9 of RFLAGS is IF, the interrupt-enable flag.
    let was_enabled = (flags & (1 << 9)) != 0;

    if was_enabled {
        unsafe { asm!("cli", options(nomem, nostack)) };
    }

    let result = f();

    if was_enabled {
        unsafe { asm!("sti", options(nomem, nostack)) };
    }

    result
}

/// Enable interrupts.
///
/// # Safety
/// Every vector that can now fire must have a handler installed, and those
/// handlers must be correct. Enabling interrupts with an incomplete IDT
/// escalates the first IRQ into a double fault.
pub unsafe fn enable() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Halt until the next interrupt.
///
/// Note the deliberate absence of `nomem`: `hlt` returns when an interrupt
/// has been serviced, and that handler may well have written memory this
/// caller is about to read. Promising `nomem` here would license the
/// compiler to keep such a value in a register across the halt — the
/// classic idle-loop miscompile.
pub fn hlt() {
    unsafe { asm!("hlt", options(nostack)) };
}
