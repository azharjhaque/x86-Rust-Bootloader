//! The 8042 PS/2 controller.
//!
//! Port `0x60` is data; `0x64` is command on write and status on read.
//! Status bit 0 means the output buffer holds a byte to read; bit 1 means
//! the input buffer still holds a byte the controller has not consumed, so
//! writing before it clears would lose the write.

use crate::port::{inb, outb};

const DATA: u16 = 0x60;
const COMMAND: u16 = 0x64;
const STATUS: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 0x01;
const STATUS_INPUT_FULL: u8 = 0x02;

const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_ENABLE_FIRST_PORT: u8 = 0xAE;

/// Configuration bit 0: raise IRQ1 when the first port has data.
const CONFIG_FIRST_PORT_INTERRUPT: u8 = 0x01;
/// Configuration bit 4: *disable* the first port's clock. Must be clear.
const CONFIG_FIRST_PORT_CLOCK_DISABLED: u8 = 0x10;

/// Iteration budget for every spin below. This all runs before `sti`, with
/// no timer tick available to bound a wait by, so each loop caps itself on
/// iteration count instead.
///
/// On a machine with no 8042 at all (common on modern UEFI-only laptops;
/// reproducible under QEMU with `-machine ...,i8042=off`), port `0x64`
/// reads back `0xFF`: both status bits appear permanently set, so an
/// unbounded "wait for input-empty" or "wait for output-full" loop spins
/// forever and interrupts are never enabled — a silent hang before the
/// kernel has any way to report one, with the harness's 60s timeout then
/// blaming whatever the last `kprintln!` happened to be (`PIT programmed at
/// 100 Hz`), not the 8042. A few hundred thousand iterations is ample time
/// for real hardware/firmware to respond, and still resolves promptly when
/// there is nothing to wait for.
const SPIN_BUDGET: u32 = 200_000;

/// Spin until the controller can accept a write, or the budget runs out.
///
/// Returns `true` if the controller became writable, `false` on timeout.
unsafe fn wait_writable() -> bool {
    for _ in 0..SPIN_BUDGET {
        if unsafe { inb(STATUS) } & STATUS_INPUT_FULL == 0 {
            return true;
        }
    }
    false
}

/// Spin until the controller has a byte for us, or the budget runs out.
///
/// Returns `true` if a byte became available, `false` on timeout.
unsafe fn wait_readable() -> bool {
    for _ in 0..SPIN_BUDGET {
        if unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL != 0 {
            return true;
        }
    }
    false
}

/// Initialise the controller so keystrokes raise IRQ1.
///
/// Never hangs: every wait below is bounded, and on timeout this reports
/// which step failed via `kprintln!` and returns rather than spinning
/// forever. The kernel is already prepared for the keyboard never working —
/// `kernel_main`'s keyboard deadline reports that cleanly — so degrading
/// here instead of completing initialisation costs nothing beyond that same,
/// already-handled failure mode, and turns a mystery boot hang into a named
/// diagnostic.
///
/// # Safety
/// Call once, with interrupts disabled, before `sti`.
pub unsafe fn init() {
    unsafe {
        // Firmware may have left a byte unread. The controller will not
        // signal again while its output buffer is full, so drain it —
        // bounded too, since a controllerless machine reads STATUS as
        // 0xFF and would otherwise never see the empty bit.
        let mut drained = false;
        for _ in 0..SPIN_BUDGET {
            if inb(STATUS) & STATUS_OUTPUT_FULL == 0 {
                drained = true;
                break;
            }
            let _ = inb(DATA);
        }
        if !drained {
            crate::kprintln!("ps2: timed out draining the output buffer; no 8042 present?");
            return;
        }

        if !wait_writable() {
            crate::kprintln!("ps2: timed out waiting to enable the first port");
            return;
        }
        outb(COMMAND, CMD_ENABLE_FIRST_PORT);

        if !wait_writable() {
            crate::kprintln!("ps2: timed out waiting to request the config byte");
            return;
        }
        outb(COMMAND, CMD_READ_CONFIG);
        if !wait_readable() {
            crate::kprintln!("ps2: timed out waiting to read the config byte");
            return;
        }
        let mut config = inb(DATA);

        config |= CONFIG_FIRST_PORT_INTERRUPT;
        config &= !CONFIG_FIRST_PORT_CLOCK_DISABLED;

        // Write it back even when nothing changed. Under OVMF the byte
        // already reads back correct, and writing it anyway is what makes
        // the controller actually deliver interrupts — verified by testing
        // the variant that skips this, which does not work.
        if !wait_writable() {
            crate::kprintln!("ps2: timed out waiting to write the config byte command");
            return;
        }
        outb(COMMAND, CMD_WRITE_CONFIG);
        if !wait_writable() {
            crate::kprintln!("ps2: timed out waiting to write the config byte");
            return;
        }
        outb(DATA, config);
    }
}
