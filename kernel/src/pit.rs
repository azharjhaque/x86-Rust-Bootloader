//! Channel 0 of the 8253/8254 programmable interval timer.
//!
//! The chip counts down from a divisor at a fixed input frequency and
//! raises IRQ0 at zero. Choosing the divisor chooses the tick rate.

use crate::port::outb;

const CHANNEL0_DATA: u16 = 0x40;
const COMMAND: u16 = 0x43;

/// The PIT's input clock: 1.193182 MHz, a number inherited from the
/// original IBM PC's crystal.
const BASE_FREQUENCY: u32 = 1_193_182;

/// Program channel 0 to raise IRQ0 at roughly `hz` times per second.
///
/// # Safety
/// Call once, with interrupts disabled. `hz` must be in `19..=BASE_FREQUENCY`
/// — below ~18.2 Hz the divisor no longer fits in the chip's 16-bit
/// counter, and 0 Hz would divide by zero.
pub unsafe fn init(hz: u32) {
    // Below ~18.2 Hz the divisor exceeds u16 and the `as` cast would wrap
    // silently — init(10) would give ~22 Hz, not 10.
    assert!(hz >= 19 && hz <= BASE_FREQUENCY, "PIT frequency out of range");

    // Integer division truncates, so the real rate is slightly above `hz`.
    // At 100 Hz the divisor is 11931 and the error is under 0.01%, which is
    // irrelevant for a tick counter.
    let divisor = (BASE_FREQUENCY / hz) as u16;

    unsafe {
        // 0x36 = channel 0, access mode lobyte/hibyte, mode 3 (square wave),
        // binary counting.
        outb(COMMAND, 0x36);
        outb(CHANNEL0_DATA, (divisor & 0xFF) as u8);
        outb(CHANNEL0_DATA, (divisor >> 8) as u8);
    }
}
