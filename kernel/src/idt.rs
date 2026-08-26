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

use crate::gdt::{DescriptorTablePointer, KERNEL_CODE_SELECTOR};

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
            // This is a real `assert!`, not `debug_assert!`: it runs once
            // at init time, costs nothing, and a release build silently
            // wrapping index 7 to "no IST" is exactly the failure mode the
            // comment above warns about.
            Some(index) => {
                assert!(index < 7, "IST index out of range");
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

/// Install a handler that runs on the current stack.
///
/// # Safety
/// `handler` must be a valid `extern "x86-interrupt"` function for `vector`.
unsafe fn set_handler(vector: u8, handler: u64) {
    unsafe { IDT.entries[vector as usize].set(handler, None) }
}

/// Install a handler that runs on a dedicated IST stack.
///
/// # Safety
/// `handler` must be a valid `extern "x86-interrupt"` function for
/// `vector`, and `ist_index` must name a slot the TSS has filled in.
unsafe fn set_handler_with_ist(vector: u8, handler: u64, ist_index: u8) {
    unsafe { IDT.entries[vector as usize].set(handler, Some(ist_index)) }
}

/// Load the table into the CPU.
///
/// # Safety
/// Every entry marked present must point at a valid handler.
unsafe fn load() {
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
extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    crate::kprintln!(
        "EXCEPTION: breakpoint at {:#x} (execution will resume)",
        frame.instruction_pointer
    );
}

/// Vector 8, raised when the CPU fails to deliver an earlier exception —
/// for example because that exception's IDT entry is absent.
///
/// It cannot return: the state that caused it has not been repaired, so
/// `iretq` would simply fault again. The error code is architecturally
/// always zero and exists only to keep the stack frame uniform.
///
/// Catching this matters because the alternative is a *triple* fault: a
/// failure to deliver the double fault, which the CPU responds to by
/// resetting the machine. A triple fault gives no diagnostics at all —
/// under QEMU's `-no-reboot` it is simply a dead VM.
///
/// This handler is reached by *any* unhandled exception, not just the one
/// `main.rs` deliberately provokes with `ud2`. It only reports success when
/// both `crate::expecting_double_fault()` is true (set immediately before
/// the deliberate `ud2`) and the IST switch is confirmed to have happened;
/// any other double fault — a real bug escalating through a missing
/// handler, or a broken IST setup — reports failure instead. Without this
/// check the handler would report success unconditionally and the harness
/// could never fail.
extern "x86-interrupt" fn double_fault_handler(
    frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    // Read the live stack pointer. This is the one that proves the IST
    // switch happened: `frame.stack_pointer` below is the *interrupted*
    // code's RSP, because the CPU loads RSP from the IST entry and then
    // pushes the old SS:RSP onto that new stack. The two are supposed to
    // differ — that difference is the whole point of an IST gate.
    let handler_rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) handler_rsp, options(nomem, nostack, preserves_flags)) };

    let (fault_lo, fault_hi) = crate::gdt::fault_stack_range();

    crate::kprintln!("EXCEPTION: double fault");
    crate::kprintln!("  faulting instruction: {:#x}", frame.instruction_pointer);
    crate::kprintln!("  interrupted stack:    {:#x}", frame.stack_pointer);
    crate::kprintln!("  handler stack:        {:#x}", handler_rsp);
    crate::kprintln!("  fault stack spans:    {fault_lo:#x}..{fault_hi:#x}");
    let on_ist = handler_rsp >= fault_lo && handler_rsp < fault_hi;
    if on_ist {
        crate::kprintln!("  handler is running on the IST stack — the machine did not reset");
    } else {
        crate::kprintln!("  WARNING: handler is NOT on the IST stack; the switch did not happen");
    }

    if !crate::expecting_double_fault() {
        crate::kprintln!("  this double fault was NOT expected — some earlier vector is unhandled");
        crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Failed);
    }
    if !on_ist {
        crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Failed);
    }
    crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Success)
}

/// Install the handlers this milestone provides and load the table.
///
/// # Safety
/// Call once, after [`crate::gdt::init`] — the entries reference the code
/// selector that function installs.
pub unsafe fn init() {
    unsafe {
        set_handler(3, breakpoint_handler as *const () as u64);
        set_handler_with_ist(
            8,
            double_fault_handler as *const () as u64,
            crate::gdt::DOUBLE_FAULT_IST_INDEX,
        );
        load();
    }
}
