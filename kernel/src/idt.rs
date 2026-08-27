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

/// Catch-all for any PIC vector without a specific handler.
///
/// Registered across the whole remapped range before the real handlers
/// overwrite their own slots, so no unmasked line can ever reach a
/// non-present gate. That matters because a missing gate does not fail
/// quietly: the CPU raises #GP, which `report_fault` turns into a halt —
/// so one stray keystroke would kill the kernel.
///
/// Returns rather than halting: a spurious IRQ7 is a normal event, not a
/// bug, and the right response is to acknowledge it and carry on.
///
/// The EOI below is unconditional and goes to both chips regardless of
/// which vector actually landed here. That is not a textbook spurious-IRQ7
/// handler — a genuine spurious IRQ7 sets no bit in the master's ISR and
/// architecturally should get *no* EOI at all, since one would falsely
/// acknowledge whatever real IRQ is next in line. This is safe only because
/// nesting is impossible right now (only IRQ0 and IRQ1 are unmasked, and
/// handlers run with IF clear), so there is never a "next IRQ in line" to
/// clobber. It will need to become vector-aware, and check the ISR before
/// sending anything, before more lines are unmasked.
extern "x86-interrupt" fn unhandled_irq_handler(_frame: InterruptStackFrame) {
    // Deliberately silent. This can fire repeatedly (a held key with no
    // driver, a spurious IRQ7), and a print per occurrence would bury the
    // real trace.
    unsafe { crate::pic::end_of_interrupt(crate::pic::PIC2_OFFSET + 7) };
}

/// Ticks counted since interrupts were enabled.
///
/// `static mut` rather than an atomic because this kernel is single-core:
/// an aligned `u64` load or store cannot tear on x86-64 on its own, so
/// atomicity of a single access was never the issue. What `static mut`
/// needs instead is mutual exclusion against the timer handler's
/// read-modify-write (`TICKS += 1`), which is not a single atomic
/// operation. The handler gets that for free because the IDT gate is an
/// *interrupt* gate — IF is clear on entry, so the handler cannot be
/// preempted by itself. Readers use `without_interrupts` to get the same
/// exclusion against a `TICKS += 1` that is mid-flight.
///
/// `without_interrupts`'s `cli`/`sti` deliberately omit `options(nomem)`,
/// which makes each one a compiler barrier: the compiler cannot reorder
/// ordinary memory accesses across them. That is what makes `f`'s accesses
/// actually happen inside the critical section instead of merely inside
/// the instructions that toggle IF — needed once Milestone 5 protects
/// multi-word state with this same function, though for the single aligned
/// word read here it would have been harmless either way.
static mut TICKS: u64 = 0;

/// The number of timer ticks so far.
pub fn ticks() -> u64 {
    crate::interrupts::without_interrupts(|| unsafe { TICKS })
}

extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    unsafe { TICKS += 1 };
    unsafe { crate::pic::end_of_interrupt(crate::pic::TIMER_VECTOR) };
}

/// Translated key events received since interrupts were enabled.
///
/// Same reasoning as `TICKS` above: single-core, single aligned `u64`, so
/// the interrupt gate's implicit `cli` on entry is what actually protects
/// the read-modify-write in `keyboard_handler`, and `without_interrupts`
/// gives readers the same exclusion.
static mut KEY_EVENTS_SEEN: u64 = 0;

/// The number of translated key events received so far.
///
/// Named "events", not "keypresses" or "presses": this counts every
/// scancode `keyboard::read_key` successfully translates, with no `0xE0`
/// extended-code handling and no repeat filtering, so one physical key held
/// down or one that requires an extended scancode does not map 1:1 to this
/// count.
pub fn key_events_seen() -> u64 {
    crate::interrupts::without_interrupts(|| unsafe { KEY_EVENTS_SEEN })
}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    // The byte must be read whether or not we can translate it: leaving it
    // in the controller's output buffer means no further keyboard IRQ ever
    // arrives.
    if let Some(ascii) = unsafe { crate::keyboard::read_key() } {
        unsafe { KEY_EVENTS_SEEN += 1 };
        crate::kprintln!("key: {:?}", ascii as char);
    }
    unsafe { crate::pic::end_of_interrupt(crate::pic::KEYBOARD_VECTOR) };
}

/// Print a uniform report for a CPU exception and stop.
///
/// Every fault handler funnels through this so the output format is one
/// thing rather than N slightly different ones, and so adding a register
/// dump later is a single edit. `cr2` is the page fault's faulting address,
/// printed as part of the same report right after the exception name.
/// Threading it through here (rather than having `page_fault_handler` print
/// it itself before calling this) keeps the report in the order a reader
/// expects: previously the caller's own `kprintln!()` header printed CR2
/// *before* this function's "EXCEPTION: ..." line, so the faulting address
/// appeared above the name of the exception it belonged to, with stray
/// blank lines from the two separate headers.
fn report_fault(name: &str, frame: &InterruptStackFrame, error_code: Option<u64>, cr2: Option<u64>) -> ! {
    crate::kprintln!();
    crate::kprintln!("EXCEPTION: {name}");
    if let Some(addr) = cr2 {
        crate::kprintln!("  faulting address (CR2): {addr:#x}");
    }
    crate::kprintln!("  instruction pointer: {:#x}", frame.instruction_pointer);
    crate::kprintln!("  code segment:        {:#x}", frame.code_segment);
    crate::kprintln!("  cpu flags:           {:#x}", frame.cpu_flags);
    crate::kprintln!("  stack pointer:       {:#x}", frame.stack_pointer);
    if let Some(code) = error_code {
        crate::kprintln!("  error code:          {code:#x}");
    }
    crate::kprintln!("  unrecoverable - halting");

    crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Failed)
}

extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) -> ! {
    report_fault("divide error (#DE)", &frame, None, None)
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) -> ! {
    report_fault("invalid opcode (#UD)", &frame, None, None)
}

extern "x86-interrupt" fn general_protection_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    report_fault("general protection fault (#GP)", &frame, Some(error_code), None)
}

/// Page faults also set CR2 to the offending address. Reading it is the
/// single most useful thing this handler can do, and it costs one
/// instruction.
extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, error_code: u64) -> ! {
    let cr2: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    report_fault("page fault (#PF)", &frame, Some(error_code), Some(cr2))
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
/// Now that the common CPU exceptions (`#DE`, `#UD`, `#GP`, `#PF`) all have
/// their own handlers, nothing in the kernel deliberately provokes a double
/// fault any more, so reaching this handler at all means some other vector
/// escalated unexpectedly. It keeps its dual-stack diagnostic — confirming
/// the IST switch happened is still genuinely useful — but the verdict is
/// now unconditional: any double fault is a failure.
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
    if handler_rsp >= fault_lo && handler_rsp < fault_hi {
        crate::kprintln!("  handler ran on the IST stack - the machine did not reset");
    } else {
        crate::kprintln!("  WARNING: handler is NOT on the IST stack; the switch did not happen");
    }

    crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Failed)
}

/// Install the handlers this milestone provides and load the table.
///
/// # Safety
/// Call once, after [`crate::gdt::init`] — the entries reference the code
/// selector that function installs.
pub unsafe fn init() {
    unsafe {
        // Gate the entire remapped PIC range first. The specific handlers
        // below overwrite their own vectors; everything else lands here
        // rather than on a non-present gate.
        for vector in crate::pic::PIC1_OFFSET..=(crate::pic::PIC2_OFFSET + 7) {
            set_handler(vector, unhandled_irq_handler as *const () as u64);
        }

        set_handler(0, divide_error_handler as *const () as u64);
        set_handler(3, breakpoint_handler as *const () as u64);
        set_handler(6, invalid_opcode_handler as *const () as u64);
        set_handler(13, general_protection_handler as *const () as u64);
        set_handler(14, page_fault_handler as *const () as u64);
        set_handler_with_ist(
            8,
            double_fault_handler as *const () as u64,
            crate::gdt::DOUBLE_FAULT_IST_INDEX,
        );
        set_handler(crate::pic::TIMER_VECTOR, timer_handler as *const () as u64);
        set_handler(crate::pic::KEYBOARD_VECTOR, keyboard_handler as *const () as u64);
        load();
    }
}
