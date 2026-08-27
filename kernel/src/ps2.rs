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

/// Spin until the controller can accept a write.
unsafe fn wait_writable() {
    while unsafe { inb(STATUS) } & STATUS_INPUT_FULL != 0 {}
}

/// Spin until the controller has a byte for us.
unsafe fn wait_readable() {
    while unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL == 0 {}
}

/// Initialise the controller so keystrokes raise IRQ1.
///
/// # Safety
/// Call once, with interrupts disabled, before `sti`.
pub unsafe fn init() {
    unsafe {
        // Firmware may have left a byte unread. The controller will not
        // signal again while its output buffer is full, so drain it.
        while inb(STATUS) & STATUS_OUTPUT_FULL != 0 {
            let _ = inb(DATA);
        }

        wait_writable();
        outb(COMMAND, CMD_ENABLE_FIRST_PORT);

        wait_writable();
        outb(COMMAND, CMD_READ_CONFIG);
        wait_readable();
        let mut config = inb(DATA);

        config |= CONFIG_FIRST_PORT_INTERRUPT;
        config &= !CONFIG_FIRST_PORT_CLOCK_DISABLED;

        // Write it back even when nothing changed. Under OVMF the byte
        // already reads back correct, and writing it anyway is what makes
        // the controller actually deliver interrupts — verified by testing
        // the variant that skips this, which does not work.
        wait_writable();
        outb(COMMAND, CMD_WRITE_CONFIG);
        wait_writable();
        outb(DATA, config);
    }
}
