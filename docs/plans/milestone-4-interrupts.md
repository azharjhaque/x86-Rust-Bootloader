# Milestone 4: Hardware Interrupts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the kernel respond to the outside world. Restructure `_start`
into a real `kernel_main`, add catch-all exception reporting, make printing
safe against preemption, remap the 8259 PICs away from the CPU's exception
vectors, drive the PIT at 100 Hz, enable interrupts for the first time, and
echo PS/2 keystrokes — with the keyboard driven automatically from QEMU's
monitor so the test can actually fail.

**Architecture:** Four new kernel modules. `interrupts` holds
`without_interrupts` and `enable`. `pic` remaps the two 8259s to vectors
32-47 and provides end-of-interrupt. `pit` programs channel 0 as a 100 Hz
square wave. `keyboard` translates scancode set 1 and echoes. `idt` grows
handlers for the common CPU exceptions plus IRQ0 and IRQ1. `xtask` gains a
QEMU monitor connection so it can inject keystrokes.

**Tech Stack:** Rust nightly (pinned `nightly-2026-08-23`),
`x86_64-unknown-none`, inline `core::arch::asm!` for every port write and
interrupt-flag change. QEMU + OVMF, plus QEMU's unix-socket monitor.

**Spec:** [docs/design.md](../design.md)

## Global Constraints

- Rust **nightly**, pinned to `nightly-2026-08-23`.
- **No crates.io dependencies in `kernel`** — not `x86_64`, not `pic8259`,
  not `pc-keyboard`. Every port write, descriptor, and scancode mapping is
  built by hand. `kernel` may depend only on the local `boot_info`.
- `xtask` stays dependency-free (`std` only). The QEMU monitor connection
  uses `std::os::unix::net::UnixStream` — no crate.
- QEMU + OVMF is the only tested target.
- All assembly is inline `core::arch::asm!` — no external assembler.
- Edition 2024; Rust 2024 requires `#[unsafe(no_mangle)]`.
- Exit-code contract unchanged: `Success = 0x10` → 33, `Failed = 0x11` → 35.
- **No page tables in this milestone.** The kernel still runs identity-mapped
  on UEFI's tables, linked at 2 MiB.
- The double-fault handler and its IST stack from Milestone 3 stay exactly as
  they are.

## Design decisions for this milestone

**Framebuffer text rendering is deferred.** The spec lists it alongside the
timer and keyboard for this milestone. It is being split off because it
shares nothing with interrupt handling: it needs a bitmap font and a glyph
blitter, and it would roughly double this milestone's size while teaching
none of the same lessons. Keyboard echo goes to serial, which proves the IRQ
path just as well. Text rendering is a self-contained piece that can land in
Milestone 6 alongside the write-up, when there is a reason to want output on
the screen rather than the wire.

**The `ud2` scaffolding is removed, the `int3` check stays.** Milestone 3
ended by deliberately double-faulting, which was the right proof then and is
fatal now — the kernel has to keep running to service interrupts. The
breakpoint check is recoverable, so it stays as a boot-time self-test. The
double-fault handler remains installed and remains the safety net; it is
simply no longer provoked on every run. This is a real reduction in
regression coverage, taken knowingly: Milestone 3's review verified the
handler, and re-proving it every boot would mean never reaching an idle loop.

**Printing must become interrupt-safe before `sti`.** `_print` has no lock.
Once interrupts are on, a timer handler that prints can preempt a `kprintln!`
already in progress and interleave bytes mid-format. Task 2 wraps `_print` in
`without_interrupts`, which is the whole fix on a single-core kernel with no
threads — there is no other execution context to race with.

**Every CPU exception gets a handler before `sti`.** Today only vectors 3 and
8 are present, so anything else escalates to a double fault. With interrupts
enabled that becomes far more likely, and "double fault" is a much worse
diagnostic than "general protection fault at RIP=...". Task 1 installs
reporting handlers for the common faults first.

**The keyboard is tested by machine, not by hand.** QEMU's monitor accepts
`sendkey` over a unix socket alongside `-serial stdio` — verified working
before this plan was written. `xtask` opens that socket and injects
keystrokes, and the kernel refuses to report success until it has seen one.
A manual "type something and look" step would not fail when the IRQ path
breaks; this does.

---

### Task 1: Restructure, and report every exception

**Files:**
- Modify: `kernel/src/main.rs`
- Modify: `kernel/src/idt.rs`

**Interfaces:**
- Produces: `kernel_main(&BootInfo) -> !` as the kernel's real body;
  `idt::init()` installing reporting handlers for the common CPU exceptions.
  Tasks 3 and 4 add IRQ handlers alongside them.

- [ ] **Step 1: Add a shared exception reporter to `idt.rs`**

Add near the other handlers. This is the function every new handler calls, so
the format stays consistent and there is one place to improve it:

```rust
/// Print a uniform report for a CPU exception and stop.
///
/// Every fault handler funnels through this so the output format is one
/// thing rather than N slightly different ones, and so adding a register
/// dump later is a single edit.
fn report_fault(name: &str, frame: &InterruptStackFrame, error_code: Option<u64>) -> ! {
    crate::kprintln!();
    crate::kprintln!("EXCEPTION: {name}");
    crate::kprintln!("  instruction pointer: {:#x}", frame.instruction_pointer);
    crate::kprintln!("  code segment:        {:#x}", frame.code_segment);
    crate::kprintln!("  cpu flags:           {:#x}", frame.cpu_flags);
    crate::kprintln!("  stack pointer:       {:#x}", frame.stack_pointer);
    if let Some(code) = error_code {
        crate::kprintln!("  error code:          {code:#x}");
    }
    crate::kprintln!("  unrecoverable — halting");

    crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Failed)
}
```

- [ ] **Step 2: Add handlers for the common faults**

Below `report_fault`:

```rust
extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) -> ! {
    report_fault("divide error (#DE)", &frame, None)
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) -> ! {
    report_fault("invalid opcode (#UD)", &frame, None)
}

extern "x86-interrupt" fn general_protection_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    report_fault("general protection fault (#GP)", &frame, Some(error_code))
}

/// Page faults also set CR2 to the offending address. Reading it is the
/// single most useful thing this handler can do, and it costs one
/// instruction.
extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, error_code: u64) -> ! {
    let cr2: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    crate::kprintln!();
    crate::kprintln!("  faulting address (CR2): {cr2:#x}");
    report_fault("page fault (#PF)", &frame, Some(error_code))
}
```

- [ ] **Step 3: Register them in `idt::init`**

Alongside the existing vector 3 and vector 8 registrations:

```rust
        set_handler(0, divide_error_handler as *const () as u64);
        set_handler(6, invalid_opcode_handler as *const () as u64);
        set_handler(13, general_protection_handler as *const () as u64);
        set_handler(14, page_fault_handler as *const () as u64);
```

Note these use the plain `as *const () as u64` form — a direct
function-item-to-integer cast trips `function_casts_as_integer`, and this
project treats warnings as defects.

- [ ] **Step 4: Restructure `main.rs`**

`_start` becomes a thin validating shim; the body moves to `kernel_main`.
Replace the current `_start` and its trailing scaffolding with:

```rust
/// The kernel's entry point, reached by a `jmp` from the bootloader with
/// `BootInfo` in `rdi` and a stack the bootloader allocated.
///
/// # Safety
/// Called exactly once, by the bootloader, with a valid `BootInfo` pointer.
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    // Serial first: from here on every failure can announce itself,
    // including the validation immediately below.
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

    kernel_main(info)
}

/// Everything the kernel does, once `BootInfo` is known good.
fn kernel_main(info: &BootInfo) -> ! {
    kprintln!(
        "framebuffer: {}x{} stride={} @ {:#x}",
        info.framebuffer.width,
        info.framebuffer.height,
        info.framebuffer.stride,
        info.framebuffer.addr
    );
    kprintln!("kernel image: base={:#x} size={:#x}", info.kernel_base, info.kernel_size);

    init();
    selftest();

    let (skipped, fell_back) = fill_screen(info, 0x00, 0x33, 0x99);
    kprintln!("framebuffer painted");
    if fell_back {
        kprintln!("  note: pixel format was not recognised; assumed BGR");
    }
    if skipped > 0 {
        kprintln!("  note: {skipped} pixels skipped as out of bounds");
    }

    kprintln!("kernel initialised");
    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}

/// One-time CPU and device setup.
fn init() {
    unsafe { gdt::init() };
    kprintln!("GDT + TSS loaded (code selector {:#x})", gdt::KERNEL_CODE_SELECTOR);

    unsafe { idt::init() };
    kprintln!("IDT loaded");
}

/// Boot-time checks that the tables `init` installed actually work.
///
/// The breakpoint is a *trap*: the CPU resumes at the following
/// instruction, so reaching the line after it proves the handler both fired
/// and returned correctly through `iretq`.
fn selftest() {
    unsafe { core::arch::asm!("int3") };
    kprintln!("selftest: breakpoint handled and execution resumed");
}
```

Delete the `EXPECTING_DOUBLE_FAULT` static, `expecting_double_fault()`, the
`ud2` block, and the `FATAL: ud2 did not fault` fallback. The double-fault
handler stays in `idt.rs`, but nothing provokes it now, so its expectation
gate is no longer meaningful — simplify it back to reporting the fault and
exiting `Failed`, since any double fault reaching it is now genuinely
unexpected.

- [ ] **Step 5: Simplify the double-fault handler**

In `idt.rs`, `double_fault_handler` keeps its dual-stack diagnostic (it is
genuinely useful) but its verdict changes: an unprovoked double fault is
always a failure now.

```rust
    if handler_rsp >= fault_lo && handler_rsp < fault_hi {
        crate::kprintln!("  handler ran on the IST stack — the machine did not reset");
    } else {
        crate::kprintln!("  WARNING: handler is NOT on the IST stack; the switch did not happen");
    }

    crate::qemu_exit::exit(crate::qemu_exit::QemuExitCode::Failed)
```

- [ ] **Step 6: Run and confirm the restructure is behaviour-preserving**

```bash
cargo xtask run
```

Expected: the same trace as before, minus the `ud2` section, ending:

```
selftest: breakpoint handled and execution resumed
framebuffer painted
kernel initialised
PASS: bootloader exited with expected code 33
```

- [ ] **Step 7: Prove the new fault handlers actually report**

Temporarily add `unsafe { core::arch::asm!("ud2") };` at the end of
`kernel_main`, run, and confirm the output is now:

```
EXCEPTION: invalid opcode (#UD)
  instruction pointer: 0x...
  ...
  unrecoverable — halting
FAIL: bootloader reported an internal failure (exit 35)
```

That is the improvement this task exists for: the same instruction that
previously produced an opaque double fault now names the exception. Remove
the temporary line afterwards and confirm PASS returns. Report both
observations.

- [ ] **Step 8: Confirm the negative test still works**

```bash
cargo xtask test
```

Expected: still exit 35.

- [ ] **Step 9: Commit**

```bash
git add kernel/src/main.rs kernel/src/idt.rs
git commit -m "Restructure into kernel_main and report CPU exceptions by name"
```

---

### Task 2: Interrupt-safe printing and `io_wait`

**Files:**
- Create: `kernel/src/interrupts.rs`
- Modify: `kernel/src/port.rs`
- Modify: `kernel/src/serial.rs`
- Modify: `kernel/src/main.rs`

**Interfaces:**
- Produces: `interrupts::without_interrupts(f)`, `interrupts::enable()`,
  `interrupts::hlt()`, and `port::io_wait()`. Tasks 3 and 4 depend on all
  four.

- [ ] **Step 1: Write the interrupt-control module**

`kernel/src/interrupts.rs`:

```rust
//! Controlling the interrupt flag.
//!
//! This kernel is single-core with no threads, so the only thing that can
//! interrupt a sequence of instructions is an interrupt. Disabling them is
//! therefore a complete critical section — no lock is needed or would help.

use core::arch::asm;

/// Run `f` with interrupts disabled, restoring the previous state after.
///
/// Restoring the *previous* state rather than unconditionally enabling
/// matters: this is called from inside interrupt handlers, where interrupts
/// are already off, and blindly running `sti` there would allow reentrancy
/// the handler is not written for.
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let flags: u64;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) };

    // Bit 9 of RFLAGS is IF, the interrupt-enable flag.
    let was_enabled = (flags & (1 << 9)) != 0;

    if was_enabled {
        unsafe { asm!("cli", options(nomem, nostack)) };
    }

    let result = f();

    if was_enabled {
        unsafe { asm!("sti", options(nomem, nostack)) };
    }

    result
}

/// Enable interrupts.
///
/// # Safety
/// Every vector that can now fire must have a handler installed, and those
/// handlers must be correct. Enabling interrupts with an incomplete IDT
/// escalates the first IRQ into a double fault.
pub unsafe fn enable() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Halt until the next interrupt.
///
/// Note the deliberate absence of `nomem`: `hlt` returns when an interrupt
/// has been serviced, and that handler may well have written memory this
/// caller is about to read. Promising `nomem` here would license the
/// compiler to keep such a value in a register across the halt — the
/// classic idle-loop miscompile.
pub fn hlt() {
    unsafe { asm!("hlt", options(nostack)) };
}
```

- [ ] **Step 2: Add `io_wait` to `port.rs`**

```rust
/// Waste a moment on a port nothing uses, to give a slow legacy device time
/// to latch the previous write.
///
/// The 8259 PIC's initialisation sequence needs a brief gap between
/// command writes on older hardware. Port `0x80` is the POST diagnostic
/// port: writing to it is harmless and takes roughly the right amount of
/// time. QEMU does not need this, but the sequence is wrong without it on
/// real hardware.
pub unsafe fn io_wait() {
    unsafe { outb(0x80, 0) };
}
```

- [ ] **Step 3: Make `_print` interrupt-safe**

In `kernel/src/serial.rs`, wrap the body:

```rust
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // Hold off interrupts for the whole formatted write. Without this, a
    // handler that prints can preempt a print already in progress and
    // interleave its bytes into the middle of this one's output — which
    // becomes possible the moment Milestone 4 enables interrupts.
    crate::interrupts::without_interrupts(|| {
        // Errors are impossible: `write_byte` cannot fail, so `write_str`
        // always returns Ok. Ignoring the Result keeps the macro
        // infallible, which matters because it is used from panic and
        // fault handlers where there is nothing left to report an error to.
        let _ = SerialPort.write_fmt(args);
    });
}
```

- [ ] **Step 4: Declare the module**

Add `mod interrupts;` to `kernel/src/main.rs` alongside the others.

- [ ] **Step 5: Run and confirm nothing changed yet**

```bash
cargo xtask run
```

Expected: identical output to Task 1, still PASS. Nothing enables interrupts
yet — this task only builds the tools. A behaviour change here would mean
`without_interrupts` is wrong.

- [ ] **Step 6: Commit**

```bash
git add kernel/src/interrupts.rs kernel/src/port.rs kernel/src/serial.rs kernel/src/main.rs
git commit -m "Add interrupt-control helpers and make printing preemption-safe"
```

---

### Task 3: Remap the PICs, drive the PIT, enable interrupts

**Files:**
- Create: `kernel/src/pic.rs`
- Create: `kernel/src/pit.rs`
- Modify: `kernel/src/idt.rs`
- Modify: `kernel/src/main.rs`

**Interfaces:**
- Produces: `pic::init()`, `pic::end_of_interrupt(vector)`,
  `pic::TIMER_VECTOR`, `pic::KEYBOARD_VECTOR`; `pit::init(hz)`;
  `idt::ticks()`. Task 4 adds the keyboard handler beside the timer one.

- [ ] **Step 1: Write the PIC module**

`kernel/src/pic.rs`:

```rust
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
        // something is written to handle it.
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
```

- [ ] **Step 2: Write the PIT module**

`kernel/src/pit.rs`:

```rust
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
/// Call once, with interrupts disabled.
pub unsafe fn init(hz: u32) {
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
```

- [ ] **Step 3: Add the timer handler and a tick counter to `idt.rs`**

```rust
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
extern "x86-interrupt" fn unhandled_irq_handler(_frame: InterruptStackFrame) {
    // Deliberately silent. This can fire repeatedly (a held key with no
    // driver, a spurious IRQ7), and a print per occurrence would bury the
    // real trace.
    unsafe { crate::pic::end_of_interrupt(crate::pic::PIC2_OFFSET + 7) };
}

/// Ticks counted since interrupts were enabled.
///
/// `static mut` rather than an atomic because this kernel is single-core
/// and the only writer is the timer handler, which cannot be preempted by
/// itself: the IDT gate is an *interrupt* gate, so IF is clear on entry.
/// Readers use `without_interrupts` to get a torn-free view.
static mut TICKS: u64 = 0;

/// The number of timer ticks so far.
pub fn ticks() -> u64 {
    crate::interrupts::without_interrupts(|| unsafe { TICKS })
}

extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    unsafe { TICKS += 1 };
    unsafe { crate::pic::end_of_interrupt(crate::pic::TIMER_VECTOR) };
}
```

Register it in `idt::init`. The catch-all must be registered first, across the
whole remapped PIC range, so that IRQ1 — unmasked below in Step 1's mask byte,
but with no keyboard handler until Task 4 — lands on a real (if silent) gate
instead of a non-present one. A non-present gate raises `#GP`, and this
kernel's `#GP` handler halts: without this, `sti` plus one keypress kills the
kernel.

```rust
        // Gate the entire remapped PIC range first. The specific handlers
        // below overwrite their own vectors; everything else lands here
        // rather than on a non-present gate.
        for vector in crate::pic::PIC1_OFFSET..=(crate::pic::PIC2_OFFSET + 7) {
            set_handler(vector, unhandled_irq_handler as *const () as u64);
        }

        set_handler(crate::pic::TIMER_VECTOR, timer_handler as *const () as u64);
```

- [ ] **Step 4: Wire it into `kernel_main`**

In `init()`, after `idt::init()`:

```rust
    unsafe { pic::init() };
    kprintln!("PICs remapped to vectors {}-{}", pic::PIC1_OFFSET, pic::PIC2_OFFSET + 7);

    unsafe { pit::init(TIMER_HZ) };
    kprintln!("PIT programmed at {TIMER_HZ} Hz");
```

with `const TIMER_HZ: u32 = 100;` at module level, and `mod pic; mod pit;`
declared.

Then replace the tail of `kernel_main` — everything from `kprintln!("kernel
initialised")` onward — with:

```rust
    kprintln!("enabling interrupts");
    unsafe { interrupts::enable() };

    // Wait for the timer to prove itself. If IRQ0 never arrives this loop
    // never exits, and xtask's 60-second timeout reports the hang — which
    // is the correct outcome, with the serial trace showing how far we got.
    const TICKS_REQUIRED: u64 = 100;
    while idt::ticks() < TICKS_REQUIRED {
        interrupts::hlt();
    }
    kprintln!("timer: {TICKS_REQUIRED} ticks received — IRQ0 works");

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
```

- [ ] **Step 5: Run and confirm the timer fires**

```bash
cargo xtask run
```

Expected, after the IDT line:

```
PICs remapped to vectors 32-47
PIT programmed at 100 Hz
enabling interrupts
timer: 100 ticks received — IRQ0 works
PASS: bootloader exited with expected code 33
```

At 100 Hz this takes about one second. The run completing at all is the
proof: `hlt` only returns when an interrupt is serviced, so reaching the
next line means IRQ0 arrived, was routed through the remapped vector, and
was acknowledged.

- [ ] **Step 6: Prove the EOI is load-bearing**

Temporarily comment out the `end_of_interrupt` call in `timer_handler` and
re-run. Expected: the trace stops after `enabling interrupts` and the run
ends with `FAIL: qemu timed out after 60s (boot hang?)` — the timer fires
once and the PIC never delivers another, so the tick count never reaches
100.

Restore the call and confirm PASS returns. Report both observations. This is
the check that the acknowledgement is real rather than decorative.

- [ ] **Step 7: Commit**

```bash
git add kernel/src/pic.rs kernel/src/pit.rs kernel/src/idt.rs kernel/src/main.rs
git commit -m "Remap the PICs, drive the PIT at 100 Hz, and enable interrupts"
```

---

### Task 4: PS/2 keyboard, driven from QEMU's monitor

**Files:**
- Create: `kernel/src/ps2.rs`
- Create: `kernel/src/keyboard.rs`
- Modify: `kernel/src/idt.rs`
- Modify: `kernel/src/main.rs`
- Modify: `xtask/src/main.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces: a kernel that will not report success until it has received a
  keystroke, and an `xtask` that injects one.

> **Correction, established empirically after Task 3 (this plan was wrong).**
> Unmasking IRQ1 at the PIC is **not sufficient** — the 8042 keyboard
> controller must be initialised too, or no keystroke ever produces an
> interrupt. This was found by patching the catch-all handler to print and
> injecting `sendkey a`: nothing fired.
>
> The controller was then brought up and the interrupt appeared. Isolating
> which part mattered, across repeated runs:
>
> | Sequence | IRQ1 delivered? |
> |---|---|
> | Nothing (previous plan) | no |
> | Drain the output buffer only | no |
> | Drain + `0xAE` (enable first port) | no |
> | Drain + `0xAE` + read/write the config byte | **yes**, 3/3 runs |
>
> Notably the config byte read back as `0x67` and was written back
> **unchanged** — bit 0 (first-port interrupt) was already set and bit 4
> (clock disable) already clear. So it is the *act* of writing the
> configuration byte that re-latches the controller's interrupt state, not
> any value change. Do not "optimise away" the write on the grounds that the
> value is already correct; that is precisely the variant that fails.
>
> Also worth knowing: one `sendkey a` produces **three** interrupts (make
> code, break code, and a repeat), so the kernel must tolerate more than one.
>
> Step 1a below adds the controller bring-up. It must run before `sti`.

- [ ] **Step 1a: Bring up the 8042 controller**

Create `kernel/src/ps2.rs`:

```rust
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
```

Declare `mod ps2;` in `main.rs`, and call `unsafe { ps2::init() };` inside
`init()` after `pit::init(...)` and before returning — i.e. before `sti`.
Print a confirmation line so the trace shows it ran.

- [ ] **Step 1: Write the keyboard module**

`kernel/src/keyboard.rs`:

```rust
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
```

- [ ] **Step 2: Add the handler and a keypress counter to `idt.rs`**

```rust
/// Keys received since interrupts were enabled.
static mut KEYS_SEEN: u64 = 0;

/// The number of translatable keypresses received so far.
pub fn keys_seen() -> u64 {
    crate::interrupts::without_interrupts(|| unsafe { KEYS_SEEN })
}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    // The byte must be read whether or not we can translate it: leaving it
    // in the controller's output buffer means no further keyboard IRQ ever
    // arrives.
    if let Some(ascii) = unsafe { crate::keyboard::read_key() } {
        unsafe { KEYS_SEEN += 1 };
        crate::kprintln!("key: {:?}", ascii as char);
    }
    unsafe { crate::pic::end_of_interrupt(crate::pic::KEYBOARD_VECTOR) };
}
```

Register it in `idt::init`:

```rust
        set_handler(crate::pic::KEYBOARD_VECTOR, keyboard_handler as *const () as u64);
```

This overwrites vector 33 (`KEYBOARD_VECTOR`), which Task 3's catch-all loop
pointed at `unhandled_irq_handler` as a stopgap. `set_handler` just replaces
the entry, so no separate "un-register the catch-all" step is needed — but
`KEYBOARD_VECTOR` now has a real reader, so delete the `#[expect(dead_code)]`
attribute above its definition in `pic.rs`: the `expect` was written to fire
exactly this warning once the constant became used, and its whole point is
that leaving it in place past this moment would itself produce a build
warning (`unfulfilled_lint_expectations`), not silence.

- [ ] **Step 3: Require a keypress before success**

In `kernel_main`, after the timer check, replace the final exit with:

```rust
    // Now wait for a keystroke. `xtask` injects one through QEMU's monitor;
    // a human running QEMU directly can just type. Bound the wait in ticks
    // so a dead IRQ1 reports itself rather than hanging until the harness
    // timeout — the timer is known good by this point, so it makes a
    // serviceable clock.
    kprintln!("waiting for a keypress...");
    let deadline = idt::ticks() + TIMER_HZ as u64 * 10;
    while idt::keys_seen() == 0 {
        if idt::ticks() > deadline {
            kprintln!("keyboard: no input within 10s — IRQ1 is not delivering");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
        interrupts::hlt();
    }
    kprintln!("keyboard: {} keypress(es) received — IRQ1 works", idt::keys_seen());

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
```

Note this distinguishes the two failure modes: a dead timer hangs and hits
the harness timeout; a dead keyboard reports itself and exits 35.

- [ ] **Step 4: Give `xtask` a monitor socket**

In `xtask/src/main.rs`, add to the imports:

```rust
use std::io::Write as _;
use std::os::unix::net::UnixStream;
```

Add a constant beside the others:

```rust
/// Where QEMU's monitor socket lives. `xtask` connects to this to inject
/// keystrokes, which is how the keyboard IRQ gets tested without a human.
const MONITOR_SOCKET: &str = "/tmp/rust_bl_monitor.sock";
```

In `boot_qemu`, before spawning, remove any stale socket, and add the
monitor argument:

```rust
    let _ = fs::remove_file(MONITOR_SOCKET);
```

and among the QEMU args:

```rust
        .arg("-monitor")
        .arg(format!("unix:{MONITOR_SOCKET},server,nowait"))
```

- [ ] **Step 5: Inject keystrokes while QEMU runs**

Add this function, and call it from `boot_qemu` right after the child is
spawned:

```rust
/// Type into the guest through QEMU's monitor.
///
/// Spawned on its own thread because QEMU is running concurrently: the
/// keystrokes have to arrive *after* the kernel has enabled interrupts, and
/// there is no signal for that other than time. Sending several spread out
/// over a few seconds is simpler and more robust than trying to synchronise,
/// and the kernel only needs one to arrive.
fn inject_keystrokes() {
    thread::spawn(|| {
        // Wait for QEMU to create the socket.
        let mut stream = None;
        for _ in 0..100 {
            thread::sleep(Duration::from_millis(100));
            if let Ok(s) = UnixStream::connect(MONITOR_SOCKET) {
                stream = Some(s);
                break;
            }
        }
        let Some(mut stream) = stream else {
            eprintln!("note: could not reach the QEMU monitor; no keys will be injected");
            return;
        };

        // The kernel spends about a second counting timer ticks before it
        // starts looking for input, so start after that and keep going.
        for _ in 0..10 {
            thread::sleep(Duration::from_millis(500));
            if stream.write_all(b"sendkey a\n").is_err() {
                // QEMU exited — the run is over, which is the normal way
                // this loop ends.
                return;
            }
        }
    });
}
```

- [ ] **Step 6: Run and confirm the keyboard works**

```bash
cargo xtask run
```

Expected ending:

```
timer: 100 ticks received — IRQ0 works
waiting for a keypress...
key: 'a'
keyboard: 1 keypress(es) received — IRQ1 works
PASS: bootloader exited with expected code 33
```

- [ ] **Step 7: Prove the keyboard check can fail**

Temporarily comment out the `set_handler(KEYBOARD_VECTOR, ...)` registration
and re-run. Expected: the timer still passes, then after ten seconds:

```
keyboard: no input within 10s — IRQ1 is not delivering
FAIL: bootloader reported an internal failure (exit 35)
```

Restore it and confirm PASS returns. Report both observations.

(With the handler unregistered, IRQ1 escalates through the empty vector to a
double fault, so the actual output may be the double-fault report instead —
either way it must be a FAIL, not a PASS. Report what you observe.)

- [ ] **Step 8: Confirm the negative test still works**

```bash
cargo xtask test
```

Expected: still exit 35.

- [ ] **Step 9: Update the README**

Mark roadmap item 4 complete, update the Status block, and refresh the
captured trace in the Build & run section — run `cargo xtask run` and copy
the real output rather than hand-writing it. Add a sentence noting that
`xtask` injects a keystroke through QEMU's monitor so the keyboard path is
tested automatically, and that running QEMU by hand lets you type instead.
Only after Step 6 has actually passed.

- [ ] **Step 10: Commit**

```bash
git add kernel/src/keyboard.rs kernel/src/idt.rs kernel/src/main.rs xtask/src/main.rs README.md
git commit -m "Echo PS/2 keystrokes, injected through QEMU's monitor"
```

---

## After this plan

Milestone 4 is complete when Task 4 passes. What it deliberately leaves:

- **Framebuffer text rendering**, split off as described above. It needs a
  bitmap font and a glyph blitter and shares nothing with interrupt work.
- **Shift, caps lock, and the `0xE0` extended prefix.** The scancode table
  is unshifted US only. Enough to prove IRQ1 delivers; not enough to be a
  real keyboard driver.
- **The APIC.** The 8259 is legacy hardware, long superseded, but it is
  simpler and needs no ACPI table parsing — and the bootloader does not
  currently pass the RSDP through `BootInfo`, so adding APIC support would
  mean a bootloader change and a `BOOT_INFO_VERSION` bump.
- **Anything resembling a scheduler.** `hlt` in a loop is the whole idle
  story. Milestone 5's allocator is the next prerequisite for that.
