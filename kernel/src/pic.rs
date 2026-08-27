//! The pair of 8259 programmable interrupt controllers.
//!
//! On a PC the two PICs are cascaded: the slave's output feeds the master's
//! IRQ2 line, giving 15 usable IRQs. Both power up mapped to interrupt
//! vectors 0-15, which in long mode collide head-on with the CPU's own
//! exception vectors — a timer tick would arrive as vector 8, which is
//! `#DF`. Remapping them somewhere above 31 is not optional.

use crate::port::{io_wait, outb};

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// ICW1: begin initialisation, and promise an ICW4 later.
const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
/// ICW4: 8086/88 mode (as opposed to the original 8080 mode).
const ICW4_8086: u8 = 0x01;
/// OCW2: non-specific end-of-interrupt.
const EOI: u8 = 0x20;

/// Where the master PIC's eight IRQs land. 32 is the first vector after the
/// 32 architecturally reserved exception vectors.
pub const PIC1_OFFSET: u8 = 32;
/// Where the slave PIC's eight IRQs land.
pub const PIC2_OFFSET: u8 = PIC1_OFFSET + 8;

/// IRQ0, the PIT.
pub const TIMER_VECTOR: u8 = PIC1_OFFSET;
/// IRQ1, the PS/2 keyboard.
///
/// Unused until Task 4 installs the keyboard handler. `expect` rather than
/// `allow` so the build tells us to delete this attribute the moment the
/// constant is actually referenced, instead of leaving a suppression nobody
/// notices is stale.
#[expect(dead_code)]
pub const KEYBOARD_VECTOR: u8 = PIC1_OFFSET + 1;

/// Remap both PICs and mask everything except the timer and keyboard.
///
/// # Safety
/// Call once, with interrupts disabled, before any IRQ can arrive.
pub unsafe fn init() {
    unsafe {
        // ICW1: start the initialisation sequence on both chips.
        outb(PIC1_COMMAND, ICW1_INIT | ICW1_ICW4);
        io_wait();
        outb(PIC2_COMMAND, ICW1_INIT | ICW1_ICW4);
        io_wait();

        // ICW2: the vector offset each chip's IRQs map to.
        outb(PIC1_DATA, PIC1_OFFSET);
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();

        // ICW3: wire up the cascade. The master is told there is a slave on
        // IRQ2 (bit 2); the slave is told its own cascade identity is 2.
        outb(PIC1_DATA, 1 << 2);
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();

        // ICW4: 8086 mode.
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        // OCW1: the interrupt masks. A set bit masks that IRQ. Unmask only
        // IRQ0 (timer) and IRQ1 (keyboard); everything else stays off until
        // something is written to handle it. Bit 2 here is the master's
        // cascade line, the same line ICW3 wired to the slave above — so
        // as long as it stays masked, IRQ8-15 cannot reach the CPU no
        // matter what PIC2_DATA's own mask says. Whoever first unmasks the
        // RTC or PS/2 mouse (both behind the slave) will need to clear
        // this bit too, or get silence with the cause one chip away.
        outb(PIC1_DATA, 0b1111_1100);
        outb(PIC2_DATA, 0b1111_1111);
    }
}

/// Acknowledge an interrupt so the PIC will deliver the next one.
///
/// Forgetting this is the classic "the timer fired exactly once" bug: the
/// PIC waits forever for an acknowledgement that never comes.
///
/// # Safety
/// Call exactly once per delivered IRQ, from that IRQ's handler.
pub unsafe fn end_of_interrupt(vector: u8) {
    unsafe {
        // An IRQ from the slave was relayed through the master, so both
        // chips need acknowledging — the slave first.
        if vector >= PIC2_OFFSET {
            outb(PIC2_COMMAND, EOI);
        }
        outb(PIC1_COMMAND, EOI);
    }
}
