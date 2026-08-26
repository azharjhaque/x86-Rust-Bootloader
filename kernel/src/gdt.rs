//! The kernel's own Global Descriptor Table and Task State Segment.
//!
//! Up to this point the kernel has been running on the GDT that UEFI
//! firmware installed. That table lives in memory the kernel is free to
//! reuse, so continuing to rely on it is borrowing against a loan that has
//! already been called. This module builds a replacement.
//!
//! In 64-bit long mode segmentation is mostly vestigial: base and limit
//! are ignored for code and data segments, and only a handful of bits
//! still mean anything. What the GDT is still needed for is (a) supplying
//! a valid code selector, which the IDT's gate descriptors reference, and
//! (b) anchoring a TSS, which is the only way to give an exception handler
//! its own stack.

use core::arch::asm;
use core::mem::size_of;

/// Which Interrupt Stack Table slot the double-fault handler uses.
///
/// The IST is a table of up to seven stack pointers in the TSS. An IDT
/// entry may name one, and the CPU then switches to that stack
/// unconditionally when the exception fires — regardless of what the
/// current stack pointer was. That is exactly what a double-fault handler
/// needs, since the fault it is catching may be a stack problem.
pub const DOUBLE_FAULT_IST_INDEX: u8 = 0;

/// Size of the dedicated fault stack, in bytes.
const FAULT_STACK_SIZE: usize = 4096 * 4;

/// The stack the double-fault handler runs on.
///
/// Wrapped in an aligned newtype rather than a bare `[u8; N]`: the CPU
/// aligns RSP down to 16 bytes on IST entry, so a misaligned top would
/// still work, but the invariant is load-bearing and belongs in the
/// declaration rather than in a reader's head.
#[repr(C, align(16))]
struct FaultStack([u8; FAULT_STACK_SIZE]);

/// `static mut` rather than an allocation because the kernel has no
/// allocator. It lives in `.bss`, which the bootloader's ELF loader zeroes.
static mut FAULT_STACK: FaultStack = FaultStack([0; FAULT_STACK_SIZE]);

/// The 64-bit Task State Segment.
///
/// In long mode the TSS no longer holds a task's register state — hardware
/// task switching is gone. What survives is the two stack tables: the
/// privilege stack table (used when changing privilege level) and the
/// interrupt stack table.
///
/// `packed(4)` matches the hardware layout: the `u64` fields sit at
/// 4-byte-aligned offsets, not 8.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct TaskStateSegment {
    reserved_1: u32,
    /// RSP for privilege levels 0-2.
    privilege_stack_table: [u64; 3],
    reserved_2: u64,
    /// The seven IST stack pointers.
    interrupt_stack_table: [u64; 7],
    reserved_3: u64,
    reserved_4: u16,
    /// Offset to the I/O permission bitmap. Setting it to the TSS size
    /// means "no bitmap present".
    iomap_base: u16,
}

impl TaskStateSegment {
    const fn new() -> Self {
        Self {
            reserved_1: 0,
            privilege_stack_table: [0; 3],
            reserved_2: 0,
            interrupt_stack_table: [0; 7],
            reserved_3: 0,
            reserved_4: 0,
            iomap_base: size_of::<Self>() as u16,
        }
    }
}

// 104 bytes is the architectural size of a 64-bit TSS. Plain repr(C) would
// give the u64 fields align 8, shifting IST1 from offset 36 to 40 and the
// struct to 112 — ltr would accept it and every IST slot the CPU read would
// be 4 bytes off.
const _: () = assert!(size_of::<TaskStateSegment>() == 104);

static mut TSS: TaskStateSegment = TaskStateSegment::new();

/// Number of entries in [`GDT`]: null, kernel code, kernel data, then the
/// TSS descriptor, which is 16 bytes and therefore occupies two entries.
/// Named so the `lgdt` limit below can be derived from it instead of
/// restating `[u64; 5]` independently of the array's own declaration.
const GDT_ENTRIES: usize = 5;

/// The GDT itself.
static mut GDT: [u64; GDT_ENTRIES] = [0; GDT_ENTRIES];

/// Argument to `lgdt`/`lidt`. `packed(2)` so the `u64` base sits
/// immediately after the `u16` limit with no padding, which is what the
/// instructions expect. Shared between this module and `idt.rs` — the
/// layout is identical for both tables, so there is exactly one
/// declaration rather than two copies that could drift apart.
#[repr(C, packed(2))]
pub(crate) struct DescriptorTablePointer {
    pub(crate) limit: u16,
    pub(crate) base: u64,
}

// 2-byte limit + 8-byte base, packed with no padding between them — this is
// what `lgdt`/`lidt` read directly off the stack, so a stray padding byte
// here would silently corrupt whichever table is loaded next.
const _: () = assert!(size_of::<DescriptorTablePointer>() == 10);

// Descriptor bit positions that still carry meaning in long mode.
const WRITABLE: u64 = 1 << 41;
const EXECUTABLE: u64 = 1 << 43;
/// Set for code and data segments, clear for system segments like the TSS.
const DESCRIPTOR_TYPE: u64 = 1 << 44;
const PRESENT: u64 = 1 << 47;
/// The "long mode" flag. Set on the code segment; must be clear on data.
const LONG_MODE: u64 = 1 << 53;

/// Selectors are byte offsets into the GDT. The low two bits are the
/// requested privilege level, which is 0 for everything here.
pub const KERNEL_CODE_SELECTOR: u16 = 1 * 8;
const KERNEL_DATA_SELECTOR: u16 = 2 * 8;
const TSS_SELECTOR: u16 = 3 * 8;

/// Build the GDT and TSS, then load them.
///
/// # Safety
/// Must be called exactly once, with interrupts disabled — that
/// precondition is established outside this crate entirely: the
/// bootloader's handoff stub (`bootloader/src/handoff.rs`) issues `cli`
/// before jumping to the kernel entry point, and nothing in the kernel
/// re-enables interrupts before this call. If that ever changes, this
/// function's caller must `cli` first. Reloading the code segment
/// mid-flight means a mistake here does not fault cleanly — it
/// triple-faults.
pub unsafe fn init() {
    unsafe {
        // Point IST[0] at the top of the fault stack. Stacks grow down, so
        // the pointer is one past the end of the array.
        let stack_top = (&raw const FAULT_STACK as u64) + FAULT_STACK_SIZE as u64;
        TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_top;

        GDT[0] = 0; // null descriptor, required
        GDT[1] = DESCRIPTOR_TYPE | PRESENT | EXECUTABLE | LONG_MODE;
        GDT[2] = DESCRIPTOR_TYPE | PRESENT | WRITABLE;

        let tss_base = &raw const TSS as u64;
        let tss_limit = (size_of::<TaskStateSegment>() - 1) as u64;

        // A system-segment descriptor is 16 bytes: the usual 8, plus 8 more
        // holding the upper half of the 64-bit base address.
        GDT[3] = (tss_limit & 0xFFFF)
            | ((tss_base & 0xFF_FFFF) << 16)
            | (0b1001 << 40) // type: available 64-bit TSS
            | PRESENT
            | (((tss_limit >> 16) & 0xF) << 48)
            | (((tss_base >> 24) & 0xFF) << 56);
        GDT[4] = tss_base >> 32;

        let pointer = DescriptorTablePointer {
            // Derived from `GDT_ENTRIES`, the same constant that sizes the
            // array, rather than restating `[u64; 5]` here — so growing the
            // table (e.g. a future LDT or extra TSS entry) can't silently
            // leave the limit describing the old, shorter size.
            limit: (GDT_ENTRIES * size_of::<u64>() - 1) as u16,
            base: &raw const GDT as u64,
        };

        asm!("lgdt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));

        // `lgdt` does not reload the segment registers — the CPU keeps using
        // the cached descriptors until each is reloaded. CS cannot be
        // written with `mov`, so the standard trick is a far return: push
        // the new selector and a target address, then `retfq` pops both.
        asm!(
            "push {selector}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            selector = in(reg) u64::from(KERNEL_CODE_SELECTOR),
            tmp = lateout(reg) _,
            options(preserves_flags),
        );

        // The data selectors can be written directly. In long mode DS and ES
        // are ignored for addressing, but SS still needs to be valid.
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov ss, {0:x}",
            in(reg) KERNEL_DATA_SELECTOR,
            options(nostack, preserves_flags),
        );

        // FS and GS are deliberately left untouched here: nothing in this
        // milestone uses them, so they still hold whatever selectors UEFI's
        // firmware GDT set up. That firmware GDT is not reloaded (its
        // memory is still ours to reuse, and CS/DS/ES/SS above no longer
        // reference it), so FS/GS are technically pointing at descriptors
        // that no longer exist. This becomes a real bug the day a per-CPU
        // GS-relative design shows up (Milestone 4+ territory) — at that
        // point FS/GS need their own selectors here, not before.

        // Load the task register, which is what actually makes the IST
        // usable.
        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

/// The address range of the double-fault stack, for diagnostics.
pub fn fault_stack_range() -> (u64, u64) {
    let bottom = &raw const FAULT_STACK as u64;
    (bottom, bottom + FAULT_STACK_SIZE as u64)
}
