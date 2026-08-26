//! The kernel's Interrupt Descriptor Table.
//!
//! The IDT is a 256-entry array mapping vector numbers to handlers. Vectors
//! 0-31 are CPU exceptions with fixed meanings (0 = divide error, 3 =
//! breakpoint, 8 = double fault, 14 = page fault); 32-255 are available for
//! hardware and software interrupts.
//!
//! Each entry is 16 bytes and splits the handler address across three
//! non-adjacent fields — an artefact of the format growing from 16 to 32 to
//! 64 bits without ever being redesigned.

use core::arch::asm;
use core::mem::size_of;

use crate::gdt::KERNEL_CODE_SELECTOR;

/// The stack frame the CPU pushes before entering a handler.
///
/// `#[repr(C)]` and field order both matter: this is written by hardware,
/// not by Rust.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct InterruptStackFrame {
    /// Address of the faulting or interrupted instruction.
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

/// One 16-byte IDT gate descriptor.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Entry {
    offset_low: u16,
    selector: u16,
    /// Low three bits select an IST slot; 0 means "keep the current stack".
    ist: u8,
    /// Present bit, privilege level, and gate type.
    type_attributes: u8,
    offset_middle: u16,
    offset_high: u32,
    reserved: u32,
}

/// Present, ring 0, 64-bit interrupt gate. An *interrupt* gate clears the
/// interrupt flag on entry; a trap gate would leave it set.
const INTERRUPT_GATE: u8 = 0x8E;

impl Entry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attributes: 0, // present bit clear
            offset_middle: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn set(&mut self, handler: u64, ist_index: Option<u8>) {
        self.offset_low = handler as u16;
        self.offset_middle = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.selector = KERNEL_CODE_SELECTOR;
        // The IST field is 1-based in the descriptor: 0 means "no IST", so
        // slot 0 of the table is encoded as 1.
        self.ist = match ist_index {
            // Hardware IST slots are 1-7; the gate field is 3 bits and 0
            // means "no IST". A bad index here would select an
            // uninitialized TSS slot (RSP = 0) and fault while pushing the
            // exception frame — a triple fault with nothing to show for it.
            Some(index) => {
                debug_assert!(index < 7, "IST index out of range");
                (index + 1) & 0b111
            }
            None => 0,
        };
        self.type_attributes = INTERRUPT_GATE;
        self.reserved = 0;
    }
}

/// The table itself.
#[repr(C, align(16))]
pub struct Idt {
    entries: [Entry; 256],
}

impl Idt {
    const fn new() -> Self {
        Self { entries: [Entry::missing(); 256] }
    }
}

// These are hardware layouts, not Rust's to choose. A reordered field or a
// changed type would still compile and would fault at the first exception,
// with no diagnostic. Catch it here instead.
const _: () = assert!(size_of::<Entry>() == 16);
const _: () = assert!(size_of::<Idt>() == 4096);

static mut IDT: Idt = Idt::new();

#[repr(C, packed(2))]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Install a handler that runs on the current stack.
///
/// # Safety
/// `handler` must be a valid `extern "x86-interrupt"` function for `vector`.
pub unsafe fn set_handler(vector: u8, handler: u64) {
    unsafe { IDT.entries[vector as usize].set(handler, None) }
}

/// Install a handler that runs on a dedicated IST stack.
///
/// # Safety
/// `handler` must be a valid `extern "x86-interrupt"` function for
/// `vector`, and `ist_index` must name a slot the TSS has filled in.
///
/// Unused until Task 4 registers the double-fault handler on an IST stack.
#[allow(dead_code)]
pub unsafe fn set_handler_with_ist(vector: u8, handler: u64, ist_index: u8) {
    unsafe { IDT.entries[vector as usize].set(handler, Some(ist_index)) }
}

/// Load the table into the CPU.
///
/// # Safety
/// Every entry marked present must point at a valid handler.
pub unsafe fn load() {
    unsafe {
        let pointer = DescriptorTablePointer {
            limit: (size_of::<Idt>() - 1) as u16,
            base: &raw const IDT as u64,
        };
        asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));
    }
}

/// Vector 3, raised by the one-byte `int3` instruction. Debuggers use it
/// for breakpoints. It is a *trap*: execution resumes at the instruction
/// after the `int3`, which makes it the safest possible way to prove the
/// IDT works.
pub extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    crate::kprintln!(
        "EXCEPTION: breakpoint at {:#x} (execution will resume)",
        frame.instruction_pointer
    );
}

/// Install the handlers this milestone provides and load the table.
///
/// # Safety
/// Call once, after [`crate::gdt::init`] — the entries reference the code
/// selector that function installs.
pub unsafe fn init() {
    unsafe {
        set_handler(3, breakpoint_handler as *const () as u64);
        load();
    }
}
