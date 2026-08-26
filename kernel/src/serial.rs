//! A minimal 16550 UART driver for COM1.
//!
//! This is the kernel's only output channel once `ExitBootServices` has
//! taken the UEFI console away. QEMU is launched with `-serial stdio`, so
//! everything written here lands in the terminal that ran `cargo xtask run`.

use core::fmt::{self, Write};

use crate::port::{inb, outb};

/// Base I/O port of COM1 on a PC. Fixed by convention since the IBM PC.
const COM1: u16 = 0x3F8;

// Register offsets from the base port. Several registers are multiplexed:
// which one you get depends on the DLAB bit in the line-control register.
const DATA: u16 = 0; // also divisor low byte when DLAB = 1
const INTERRUPT_ENABLE: u16 = 1; // also divisor high byte when DLAB = 1
const FIFO_CONTROL: u16 = 2;
const LINE_CONTROL: u16 = 3;
const MODEM_CONTROL: u16 = 4;
const LINE_STATUS: u16 = 5;

/// Line-status bit meaning "the transmit holding register is empty", i.e.
/// the UART is ready to accept another byte.
const LINE_STATUS_THR_EMPTY: u8 = 0x20;

/// Initialise COM1 to 38400 baud, 8 data bits, no parity, 1 stop bit.
///
/// The firmware may have left the UART in any state, so this reconfigures
/// it from scratch rather than assuming a usable baud rate.
///
/// # Safety
/// Must be called once before any other function in this module, and only
/// on a system that actually has a 16550-compatible UART at [`COM1`] —
/// true for QEMU and for real PCs with a serial port.
pub unsafe fn init() {
    unsafe {
        // Mask all UART interrupts. This driver is polled; an interrupt
        // here would vector through an IDT that does not exist yet.
        outb(COM1 + INTERRUPT_ENABLE, 0x00);

        // Set DLAB so the first two registers become the baud divisor.
        outb(COM1 + LINE_CONTROL, 0x80);
        // Divisor 3 = 115200 / 3 = 38400 baud. Low byte.
        outb(COM1 + DATA, 0x03);
        // Divisor high byte. With DLAB still set this is *not* the IER
        // write above repeated — it is register 1 reinterpreted as the top
        // half of the same 16-bit divisor written to register 0 just
        // above. Both bytes must land before DLAB is cleared below, or the
        // UART is left with only half a divisor programmed.
        outb(COM1 + INTERRUPT_ENABLE, 0x00);

        // Clear DLAB and set 8N1 in the same write.
        outb(COM1 + LINE_CONTROL, 0x03);

        // Enable and clear the FIFOs, with a 14-byte interrupt threshold.
        outb(COM1 + FIFO_CONTROL, 0xC7);

        // Assert DTR and RTS, and enable OUT2 — on a real PC OUT2 gates the
        // UART's interrupt line, and it is harmless to set here.
        outb(COM1 + MODEM_CONTROL, 0x0B);
    }
}

/// Write one byte, spinning until the UART can accept it.
///
/// A safe wrapper, even though `port.rs`'s module doc says on purpose that
/// there is no safe wrapper there — the justification lives here instead of
/// at each call site because there is exactly one call site for each port:
/// `COM1` is fixed by PC convention (see [`COM1`]), reading `LINE_STATUS`
/// has no side effects on a 16550, and writing `DATA` (the transmit holding
/// register) only queues a byte for transmission — it cannot reconfigure
/// the device the way writes to `LINE_CONTROL` or `MODEM_CONTROL` can. Both
/// preconditions depend on [`init`] having already run (DLAB clear, FIFOs
/// enabled): this must not be called before it.
fn write_byte(byte: u8) {
    // Busy-wait for the transmit holding register. At 38400 baud this is
    // microseconds; a kernel with no scheduler has nothing better to do.
    while unsafe { inb(COM1 + LINE_STATUS) } & LINE_STATUS_THR_EMPTY == 0 {}
    unsafe { outb(COM1 + DATA, byte) }
}

/// A zero-sized handle implementing [`fmt::Write`] so the formatting
/// machinery in `core` can drive the UART.
pub struct SerialPort;

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            // Terminals expect CRLF; Rust strings carry bare LF.
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}

/// Backing function for the [`kprint!`] macro. Not called directly.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // Errors are impossible: `write_byte` cannot fail, so `write_str`
    // always returns Ok. Ignoring the Result keeps the macro infallible,
    // which matters because it is used from panic and fault handlers where
    // there is nothing left to report an error to.
    let _ = SerialPort.write_fmt(args);
}

/// Print to COM1 without a trailing newline.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*))
    };
}

/// Print to COM1 with a trailing newline.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => { $crate::kprint!("{}\n", format_args!($($arg)*)) };
}
