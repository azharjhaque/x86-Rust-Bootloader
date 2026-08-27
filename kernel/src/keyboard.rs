//! A minimal PS/2 keyboard driver.
//!
//! The controller hands us one byte per event on port `0x60`. In scancode
//! set 1 — what the controller produces by default after translation — a
//! key press sends a "make" code and a release sends the same code with bit
//! 7 set. This driver ignores releases and every modifier, which is enough
//! to prove the IRQ path works; shift handling and the extended `0xE0`
//! prefix are deliberately out of scope.

use crate::port::inb;

const DATA_PORT: u16 = 0x60;

/// Scancode set 1, make codes only, unshifted US layout.
///
/// Index is the scancode; the value is the ASCII byte it produces, or 0 for
/// keys this driver does not translate (modifiers, function keys, escape).
static SCANCODE_TO_ASCII: [u8; 128] = [
    0, 0, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 8, b'\t',
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', b'\n', 0, b'a', b's',
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0, b'\\', b'z', b'x', b'c', b'v',
    b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', 0, b' ', 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Read the pending scancode and translate it.
///
/// Returns `Some(ascii)` for a translatable key press, `None` for a
/// release, a modifier, or anything else.
///
/// # Safety
/// Call only from the keyboard IRQ handler — reading the data port
/// consumes the byte, so calling it elsewhere would steal an event.
pub unsafe fn read_key() -> Option<u8> {
    let scancode = unsafe { inb(DATA_PORT) };

    // Bit 7 set means a key release. Ignore those.
    if scancode & 0x80 != 0 {
        return None;
    }

    match SCANCODE_TO_ASCII[scancode as usize] {
        0 => None,
        ascii => Some(ascii),
    }
}
