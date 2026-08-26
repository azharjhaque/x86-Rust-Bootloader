# Milestone 3: Serial, GDT, IDT, and Fault Handling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the kernel a voice and its own CPU tables. Add a COM1 serial
driver so the kernel can print after `ExitBootServices`, then replace the
firmware's GDT with one the kernel owns (including a TSS with an interrupt
stack), install an IDT, and prove exception handling works — first with a
recoverable breakpoint, then by catching a double fault that would otherwise
have triple-faulted the machine.

**Architecture:** Four new kernel modules, each one task. `serial` is a
16550 UART driver plus a `kprintln!` macro — everything after it is
debuggable. `gdt` builds a long-mode GDT and a TSS whose IST[0] points at a
dedicated fault stack, then loads both. `idt` builds a 256-entry table and
installs handlers via the `x86-interrupt` calling convention. The final task
deliberately raises `#UD` with no handler installed, forcing the CPU into a
double fault, and catches it.

**Tech Stack:** Rust nightly (pinned `nightly-2026-08-23`),
`x86_64-unknown-none`, inline `core::arch::asm!` for all port I/O and
descriptor-table loads, `#![feature(abi_x86_interrupt)]` for handler
signatures. QEMU + OVMF.

**Spec:** [docs/design.md](../design.md)

## Global Constraints

- Rust **nightly**, pinned to `nightly-2026-08-23` in `rust-toolchain.toml`.
- **No crates.io dependencies in `kernel`** — not `x86_64`, not `uart_16550`,
  not `lazy_static`. Every descriptor table and every port write is built by
  hand. This is the point of the project. `kernel` may depend only on the
  local `boot_info`.
- `xtask` stays dependency-free (`std` only).
- QEMU + OVMF is the only tested target.
- All assembly is inline `core::arch::asm!` — no external assembler.
- Edition 2024 across all crates; Rust 2024 requires `#[unsafe(no_mangle)]`.
- The exit-code contract is unchanged and load-bearing:
  `QemuExitCode::Success = 0x10` → QEMU process exit 33;
  `Failed = 0x11` → 35. Value 0 is deliberately unused.
- The kernel currently runs on UEFI's page tables, identity-mapped, linked at
  2 MiB. **This milestone does not build page tables.** Nothing here may
  depend on paging beyond what is already in place.
- Interrupts arrive disabled (the bootloader's handoff `asm!` runs `cli`).
  Nothing in this milestone re-enables them — `sti` belongs to Milestone 4,
  once there are real IRQ handlers to receive.

## Design decisions for this milestone

**Serial comes first, before the GDT.** The design spec lists Milestone 3 as
"GDT + IDT + double-fault handler". This plan inserts a serial driver ahead
of all of it, on the final review's recommendation. The reason is practical:
right now the kernel's entire observable output is one of two exit codes and
a solid blue screen. Debugging a GDT reload — where a wrong descriptor bit
triple-faults instantly and silently — through a one-bit channel is not
viable. Roughly 60 lines of port I/O makes every task after it diagnosable.

**Progress markers go to serial, not to extra exit codes.** The final review
suggested extending `QemuExitCode` into a marker table so `xtask` could
distinguish "GDT loaded" from "IDT loaded". Once serial exists that is the
worse option: an exit code carries one number and ends the run, while serial
carries an ordered trace of everything that happened before the failure.
`QemuExitCode` stays a two-value terminal verdict; the narrative goes to
COM1.

**The `x86-interrupt` calling convention, not hand-written naked stubs.**
Verified compiling on the pinned toolchain for `x86_64-unknown-none`. An
interrupt handler cannot use the normal C ABI — the CPU pushes a different
frame and the handler must end in `iretq` rather than `ret`. Writing that by
hand means a naked assembly stub per vector. `#![feature(abi_x86_interrupt)]`
lets the compiler emit it, which keeps the focus on what the descriptor
tables actually mean. This is a deliberate exception to the
"build-it-by-hand" rule, and it is the only one: the GDT, TSS, IDT, and every
port write are still assembled bit by bit.

**The double fault is triggered by `ud2`, not stack overflow.** The classic
demonstration is infinite recursion overflowing the kernel stack into a guard
page. We have no guard page — the bootloader hands over a plain 64 KiB
allocation, so an overflow would silently scribble over whatever is below it
rather than faulting. Instead Task 4 executes `ud2`, raising `#UD` (vector 6)
with no vector-6 handler installed. The CPU cannot deliver the exception, so
it escalates to `#DF` (vector 8). That is deterministic, needs no page
tables, and still exercises the IST stack switch. Guard pages and a genuine
stack-overflow test become possible once Milestone 5 builds page tables;
noted there as future work.

---

### Task 1: COM1 serial driver and `kprintln!`

**Files:**
- Create: `kernel/src/port.rs`
- Create: `kernel/src/serial.rs`
- Modify: `kernel/src/main.rs`
- Modify: `README.md`

**Interfaces:**
- Produces: `port::outb(port: u16, value: u8)` and `port::inb(port: u16) -> u8`
  (both `unsafe`), used by every later task; `serial::init()`; and the
  `kprint!` / `kprintln!` macros, which every later task uses to report
  progress.

- [ ] **Step 1: Write the port I/O primitives**

`kernel/src/port.rs`:

```rust
//! Raw x86 port I/O.
//!
//! The `in`/`out` instructions are the only way to reach legacy devices
//! like the UART and the PIC. There is no safe wrapper here on purpose:
//! writing to an arbitrary port can reconfigure hardware underneath the
//! running system, so every call site states why its port and value are
//! correct.

use core::arch::asm;

/// Write a byte to an I/O port.
///
/// # Safety
/// The caller must ensure `port` is a device that expects this write, and
/// that the write is valid for the device's current state.
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Read a byte from an I/O port.
///
/// # Safety
/// The caller must ensure `port` is a device that expects to be read, and
/// that reading it has no unwanted side effects (some device registers
/// clear-on-read).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
```

- [ ] **Step 2: Write the UART driver**

`kernel/src/serial.rs`:

```rust
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
        // Divisor 3 = 115200 / 3 = 38400 baud.
        outb(COM1 + DATA, 0x03);
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
```

- [ ] **Step 3: Wire it into the kernel entry point**

In `kernel/src/main.rs`, declare the new modules near the existing
`mod qemu_exit;`:

```rust
mod port;
mod qemu_exit;
mod serial;
```

Then, in `_start`, initialise serial as the very first action — before the
null check, so that even a bad `BootInfo` pointer can be reported:

```rust
    // Initialise serial first: from here on every failure can announce
    // itself, including the validation failures immediately below.
    unsafe { serial::init() };
    kprintln!();
    kprintln!("=== Rust_BL kernel ===");

    if boot_info.is_null() {
        kprintln!("FATAL: boot_info pointer is null");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    let info = unsafe { &*boot_info };
    if !info.is_valid() {
        kprintln!("FATAL: boot_info failed validation (magic/version/format)");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    kprintln!(
        "framebuffer: {}x{} stride={} @ {:#x}",
        info.framebuffer.width,
        info.framebuffer.height,
        info.framebuffer.stride,
        info.framebuffer.addr
    );
    kprintln!("kernel image: base={:#x} size={:#x}", info.kernel_base, info.kernel_size);
```

Keep the existing `fill_screen` call and the final
`qemu_exit::exit(QemuExitCode::Success)`, but print a line just before the
exit so the trace has an obvious end:

```rust
    fill_screen(info, 0x00, 0x33, 0x99);
    kprintln!("framebuffer painted");
    kprintln!("kernel reached the end of milestone 3 setup");

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
```

- [ ] **Step 4: Report panics over serial**

Replace the panic handler so a kernel panic says what happened instead of
silently exiting:

```rust
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // The formatting machinery allocates nothing, so this is safe even
    // here. If the UART itself is the problem this will hang in
    // `write_byte`, which is still more informative than a silent exit.
    kprintln!("KERNEL PANIC: {info}");
    qemu_exit::exit(qemu_exit::QemuExitCode::Failed)
}
```

- [ ] **Step 5: Build and run**

```bash
cargo xtask run
```

Expected: the bootloader's UEFI log lines appear as before, then — after the
`handing off to kernel` line — the kernel's own output:

```
=== Rust_BL kernel ===
framebuffer: 1280x800 stride=1280 @ 0x80000000
kernel image: base=0x200000 size=0x4000
framebuffer painted
kernel reached the end of milestone 3 setup
PASS: bootloader exited with expected code 33
```

This is the milestone's first real payoff: output from *after*
`ExitBootServices`, produced by a driver the kernel owns.

- [ ] **Step 6: Confirm the failure path still reports**

```bash
cargo xtask test
```

Expected: still `PASS: bootloader rejected the corrupted image (exit 35)`.
The kernel never runs in that scenario, so no kernel output appears — which
is itself the correct result.

- [ ] **Step 7: Update the README**

In the Roadmap, leave item 3 unchecked (this task is only part of it), but
add a line to the Build & run section noting that kernel output now appears
on the serial console after the handoff. Keep it to one or two sentences.

- [ ] **Step 8: Commit**

```bash
git add kernel/src/port.rs kernel/src/serial.rs kernel/src/main.rs README.md
git commit -m "Add COM1 serial driver so the kernel can report after ExitBootServices"
```

---

### Task 2: GDT and TSS

**Files:**
- Create: `kernel/src/gdt.rs`
- Modify: `kernel/src/main.rs`

**Interfaces:**
- Consumes: `kprintln!` from Task 1.
- Produces: `gdt::init()`, which loads the kernel's own GDT and TSS and
  returns the selectors; `gdt::KERNEL_CODE_SELECTOR` (the value Task 3 puts
  in every IDT entry); and `gdt::DOUBLE_FAULT_IST_INDEX`, the IST slot Task 4
  points its handler at.

- [ ] **Step 1: Write the GDT module**

`kernel/src/gdt.rs`:

```rust
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
/// `static mut` rather than an allocation because the kernel has no
/// allocator. It lives in `.bss`, which the bootloader's ELF loader zeroes.
static mut FAULT_STACK: [u8; FAULT_STACK_SIZE] = [0; FAULT_STACK_SIZE];

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

static mut TSS: TaskStateSegment = TaskStateSegment::new();

/// The GDT itself: null, kernel code, kernel data, then the TSS descriptor,
/// which is 16 bytes and therefore occupies two entries.
static mut GDT: [u64; 5] = [0; 5];

/// Argument to `lgdt`. `packed(2)` so the `u64` base sits immediately after
/// the `u16` limit with no padding, which is what the instruction expects.
#[repr(C, packed(2))]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

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
/// Must be called exactly once, with interrupts disabled. Reloading the
/// code segment mid-flight means a mistake here does not fault cleanly —
/// it triple-faults.
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
            limit: (size_of::<[u64; 5]>() - 1) as u16,
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

        // Load the task register, which is what actually makes the IST
        // usable.
        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}
```

- [ ] **Step 2: Call it from the kernel**

In `kernel/src/main.rs`, add `mod gdt;` and call it after the `BootInfo`
validation but before painting the framebuffer:

```rust
    unsafe { gdt::init() };
    kprintln!("GDT + TSS loaded (code selector {:#x})", gdt::KERNEL_CODE_SELECTOR);
```

- [ ] **Step 3: Run and confirm the kernel survived the reload**

```bash
cargo xtask run
```

Expected: the new `GDT + TSS loaded (code selector 0x8)` line, followed by
the rest of the trace and `PASS`.

The significant part is not the message but that anything prints *after* it.
Reloading CS with a bad descriptor triple-faults immediately, so reaching
the next line proves the descriptors are valid and the far return landed
where it was supposed to.

- [ ] **Step 4: Verify the fault stack is where the TSS says it is**

Add a temporary diagnostic line after `gdt::init()`:

```rust
    kprintln!("double-fault IST index: {}", gdt::DOUBLE_FAULT_IST_INDEX);
```

Run once and confirm it prints `0`, then leave the line in — Task 4 relies
on that slot and the trace is more useful with it visible.

- [ ] **Step 5: Commit**

```bash
git add kernel/src/gdt.rs kernel/src/main.rs
git commit -m "Build and load the kernel's own GDT and TSS"
```

---

### Task 3: IDT and a recoverable exception

**Files:**
- Create: `kernel/src/idt.rs`
- Modify: `kernel/src/main.rs`

**Interfaces:**
- Consumes: `gdt::KERNEL_CODE_SELECTOR` from Task 2, `kprintln!` from Task 1.
- Produces: `idt::init()`, and the `Idt`/`InterruptStackFrame` types plus
  `idt::set_handler_with_ist`, which Task 4 uses to install the double-fault
  handler on an IST stack.

- [ ] **Step 1: Enable the interrupt-ABI feature**

At the top of `kernel/src/main.rs`, beside the existing `#![no_std]` and
`#![no_main]`:

```rust
#![feature(abi_x86_interrupt)]
```

- [ ] **Step 2: Write the IDT module**

`kernel/src/idt.rs`:

```rust
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
            Some(index) => index + 1,
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
```

- [ ] **Step 3: Call it and trigger a breakpoint**

In `kernel/src/main.rs`, add `mod idt;` and, after the `gdt::init()` block:

```rust
    unsafe { idt::init() };
    kprintln!("IDT loaded");

    // Raise a breakpoint exception on purpose. It is a trap, so the CPU
    // resumes at the following instruction — if the next line prints, the
    // handler ran and returned correctly.
    unsafe { core::arch::asm!("int3", options(nomem, nostack)) };
    kprintln!("resumed after breakpoint");
```

- [ ] **Step 4: Run and confirm the exception was handled and returned from**

```bash
cargo xtask run
```

Expected, in order:

```
GDT + TSS loaded (code selector 0x8)
double-fault IST index: 0
IDT loaded
EXCEPTION: breakpoint at 0x2003xx (execution will resume)
resumed after breakpoint
framebuffer painted
```

Both lines matter. `EXCEPTION: breakpoint` proves the CPU found the handler
through your descriptor. `resumed after breakpoint` proves the handler
returned via `iretq` correctly and execution continued — the part that
would fail if the gate type or the stack frame layout were wrong.

- [ ] **Step 5: Commit**

```bash
git add kernel/src/idt.rs kernel/src/main.rs
git commit -m "Add an IDT and a breakpoint handler"
```

---

### Task 4: Double-fault handler on an IST stack

**Files:**
- Modify: `kernel/src/idt.rs`
- Modify: `kernel/src/main.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces: a kernel that survives a fault which would otherwise reset the
  machine.

- [ ] **Step 1: Add the double-fault handler**

In `kernel/src/idt.rs`, below `breakpoint_handler`:

```rust
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
pub extern "x86-interrupt" fn double_fault_handler(
    frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    crate::kprintln!("EXCEPTION: double fault");
    crate::kprintln!("  faulting instruction: {:#x}", frame.instruction_pointer);
    crate::kprintln!("  stack pointer:        {:#x}", frame.stack_pointer);
    crate::kprintln!("  caught on the IST stack — the machine did not reset");

    crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Success)
}
```

- [ ] **Step 2: Register it on the IST stack**

In `idt::init`, add it beside the breakpoint handler:

```rust
        set_handler(3, breakpoint_handler as *const () as u64);
        set_handler_with_ist(
            8,
            double_fault_handler as *const () as u64,
            crate::gdt::DOUBLE_FAULT_IST_INDEX,
        );
        load();
```

- [ ] **Step 3: Trigger a double fault**

In `kernel/src/main.rs`, replace the final
`kprintln!("kernel reached the end of milestone 3 setup");` and the
`qemu_exit::exit(Success)` that follows it with:

```rust
    kprintln!("kernel reached the end of milestone 3 setup");
    kprintln!();
    kprintln!("about to raise #UD with no vector-6 handler installed;");
    kprintln!("the CPU should escalate it to a double fault...");

    // `ud2` is architecturally guaranteed to raise an invalid-opcode
    // exception (#UD, vector 6). Nothing is registered for vector 6, so the
    // CPU cannot deliver it and escalates to #DF. The double-fault handler
    // exits, so control never returns here.
    unsafe { core::arch::asm!("ud2", options(nomem, nostack)) };

    kprintln!("FATAL: ud2 did not fault — the CPU ignored an invalid opcode");
    qemu_exit::exit(qemu_exit::QemuExitCode::Failed)
```

- [ ] **Step 4: Run and confirm the fault was caught, not fatal**

```bash
cargo xtask run
```

Expected ending:

```
about to raise #UD with no vector-6 handler installed;
the CPU should escalate it to a double fault...
EXCEPTION: double fault
  faulting instruction: 0x2003xx
  stack pointer:        0x...
  caught on the IST stack — the machine did not reset
PASS: bootloader exited with expected code 33
```

If instead the run ends with `FAIL: expected exit code 33, got ...` and no
double-fault output, the machine triple-faulted: the IDT entry, the IST
index, or the TSS is wrong. If it ends with the `ud2 did not fault` line,
the exception never fired at all.

- [ ] **Step 5: Confirm the IST switch actually happened**

The printed stack pointer should fall inside the fault stack, not the main
kernel stack the bootloader allocated. Add a temporary line to
`double_fault_handler` printing the expected range, run once, and confirm
the frame's `stack_pointer` lies within it:

```rust
    crate::kprintln!("  (fault stack spans {:#x}..{:#x})", crate::gdt::fault_stack_range().0, crate::gdt::fault_stack_range().1);
```

and in `kernel/src/gdt.rs`:

```rust
/// The address range of the double-fault stack, for diagnostics.
pub fn fault_stack_range() -> (u64, u64) {
    let bottom = &raw const FAULT_STACK as u64;
    (bottom, bottom + FAULT_STACK_SIZE as u64)
}
```

Once confirmed, keep both — a fault report that shows which stack it ran on
is worth the six lines.

- [ ] **Step 6: Confirm the failure path is unaffected**

```bash
cargo xtask test
```

Expected: still exit 35. The kernel never runs, so none of this milestone's
output appears.

- [ ] **Step 7: Update the README**

Mark roadmap item 3 complete and update the Status block to describe what
the kernel now does: prints over serial, installs its own GDT/TSS and IDT,
handles a breakpoint, and catches a double fault on a dedicated stack
instead of resetting the machine. Only after Step 4 has actually passed.

- [ ] **Step 8: Commit**

```bash
git add kernel/src/idt.rs kernel/src/gdt.rs kernel/src/main.rs README.md
git commit -m "Catch double faults on a dedicated IST stack"
```

---

## After this plan

Milestone 3 is complete when Task 4 passes. Two things it deliberately left
undone, so they are not surprises later:

- **No `sti`.** Interrupts are still disabled. Milestone 4 remaps the 8259
  PICs, installs timer and keyboard handlers, and only then enables
  interrupts — enabling them before there are handlers would vector
  straight into absent entries.
- **No guard page, so no genuine stack-overflow test.** The double fault
  here is raised by `ud2` rather than by exhausting the stack, because
  without page tables an overflow scribbles over memory instead of
  faulting. Once Milestone 5 builds page tables, mapping a guard page below
  the kernel stack turns stack overflow into a clean double fault, and the
  handler written here catches it unchanged.
