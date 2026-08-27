# Design: a UEFI bootloader and minimal kernel in Rust

## Goal

Build a UEFI bootloader and a small freestanding kernel for x86_64 from
scratch, in Rust, with no operating system underneath.

The point is to implement the parts most projects skip by pulling in a
crate: parsing and loading an ELF, collecting boot information from
firmware, leaving UEFI behind cleanly, setting up descriptor tables,
handling hardware interrupts, and managing physical memory. Using the
`bootloader` crate would have produced a working kernel faster and taught
almost none of that.

Target is QEMU with OVMF firmware. No physical hardware requirement.

## Scope

Built:

- A UEFI application that loads a separate kernel ELF from the EFI System
  Partition, parses and maps it, collects the memory map and framebuffer,
  exits boot services, and jumps to the kernel.
- A freestanding kernel that sets up its own GDT and IDT, handles timer and
  keyboard interrupts, renders text to the framebuffer, and manages its own
  physical memory.
- A physical frame allocator and a heap allocator, so `Box`, `Vec`, and
  `String` work inside the kernel.
- A `cargo xtask` pipeline that builds everything, stages a disk image, and
  runs it under QEMU.

Deliberately not built:

- Custom page tables. The kernel runs on the identity mapping UEFI leaves
  behind.
- A scheduler or any form of multitasking.
- A filesystem driver. Both binaries read only from the ESP.
- User mode and syscalls.
- A legacy BIOS boot path.

## Architecture

Four crates in one Cargo workspace:

```
bootloader/     UEFI application (PE32+), no_std, x86_64-unknown-uefi
kernel/         freestanding kernel ELF, no_std, x86_64-unknown-none
boot_info/      shared #[repr(C)] handoff ABI
xtask/          build automation: stages the ESP, launches QEMU
```

`boot_info` exists because the bootloader and the kernel are separately
compiled binaries that have to agree on a memory layout exactly. Making that
agreement a shared Rust type, rather than offsets written down in two
places, means a mismatch is a compile error instead of a mysterious fault at
runtime. The crate asserts its own struct sizes at compile time for the same
reason.

## Boot flow

1. OVMF loads `bootloader.efi` from the ESP and calls `efi_main`.
2. The bootloader opens the ESP, reads `kernel.elf`, and parses its program
   headers by hand. For each `PT_LOAD` segment it allocates pages at the
   segment's own virtual address and copies the data in, zeroing the
   `.bss` tail.
3. It acquires a framebuffer through the Graphics Output Protocol, then the
   UEFI memory map. The map has to come last: its key must still be valid
   when boot services exit.
4. It calls `exit_boot_services`. Every UEFI service, including the logger,
   is gone after this.
5. It disables interrupts, switches to a stack it allocated earlier, and
   jumps to the kernel entry point with a `BootInfo` pointer in `rdi`.
6. The kernel validates `BootInfo`, brings up serial output, installs its
   GDT and IDT, remaps the PICs, programs the timer, initialises the
   keyboard controller, paints the framebuffer, initialises both allocators,
   and enables interrupts.

Step 5 has two details that are easy to get wrong and hard to debug. The
`cli` is required because `exit_boot_services` leaves interrupts enabled
while `IDTR` still points into firmware memory that is about to be reused;
one interrupt in that window reads a descriptor that may no longer exist.
And the stack pointer is deliberately biased by 8, because the SysV ABI
guarantees a function sees `rsp % 16 == 8` on entry — true after a `call`,
which pushes a return address, but not after a `jmp`, which pushes nothing.

## Memory management

**Frame allocator.** Seeded from the UEFI memory map, filtered to
`CONVENTIONAL` regions. A bump cursor walks those regions; freed frames go
onto a LIFO stack whose links are written into the free frames themselves.
No side table, and no bootstrap problem — a bitmap would need storage before
any allocator exists to hand it out.

Filtering on `CONVENTIONAL` does more work than it looks like. The
bootloader allocates the kernel image, the kernel stack, `BootInfo`, and the
region array as `LOADER_DATA`, so that one check excludes everything the
kernel is still running on. There is no exclusion list to maintain or get
wrong.

**Heap.** 1 MiB, carved from 256 contiguous frames at boot. An
address-ordered free list with splitting and coalescing in both directions,
registered as the `#[global_allocator]`. Block headers live inside the free
blocks, same idea as the frame allocator. Coalescing matters more than it
sounds: `Vec` grows by reallocating at doubling sizes, and without merging
adjacent free blocks the heap fragments into slivers that fit nothing.

Both allocators protect their state with `interrupts::without_interrupts`.
On a single-core kernel with no threads, disabling interrupts is a complete
critical section — a lock would add nothing.

## Toolchain

- Rust nightly, pinned in `rust-toolchain.toml` along with both targets.
- The `uefi` crate for boot services. It is the only third-party dependency
  anywhere in the workspace.
- The kernel builds against the built-in `x86_64-unknown-none` target rather
  than a custom target JSON. It already defaults to the kernel code model,
  no red zone, and no SSE, and being Tier 2 it ships `core` and `alloc`
  precompiled, so `-Z build-std` is unnecessary.
- All assembly is inline `asm!`. No external assembler.
- `xtask` is ordinary Rust rather than shell scripts, so the build logic is
  type-checked and testable.

## Verification

There is no test harness inside a freestanding kernel, so correctness is
checked in three layers.

**Host unit tests** cover the logic that can be separated from hardware:
the memory-region arithmetic feeding the frame allocator, and the
screen-capture parsing in `xtask`. These run natively under `cargo test`.

**The boot smoke test** (`cargo xtask run`) boots the image in QEMU and
requires an exit code of 33, a framebuffer that contains real glyphs, and
both interrupt lines proven live. Keystrokes are injected through QEMU's
monitor, so no human is needed. The kernel asserts its own invariants and
exits with a failure code if any of them break, which means a regression
fails the build rather than printing a warning into a log.

**The failure-path test** (`cargo xtask test`) corrupts the kernel image and
checks that the bootloader rejects it instead of jumping into it. This
exists because the happy path never exercises a single validation branch in
the ELF loader — all of that code could be deleted and `cargo xtask run`
would still pass. The distinction is worth testing: a rejection is a logged
error and a clean exit, while a miss is a triple fault with nothing left
alive to report it.

Progress is reported on the serial console rather than through per-step exit
codes. A single byte cannot carry something like the double-fault handler's
two stack addresses, and a readable trace is more useful than a number.

## Milestones

Each was built and verified before the next began.

1. Workspace scaffold and an `xtask` that boots an empty `.efi`, proving the
   toolchain end to end before any real logic exists.
2. Bootloader: ELF loader, memory map, framebuffer, exit boot services, and
   a jump to a kernel stub that writes one pixel.
3. Kernel: GDT, IDT, double-fault handler.
4. Kernel: PIT timer and PS/2 keyboard interrupts.
5. Kernel: framebuffer text rendering, serial fan-out, and an automated
   screen-capture assertion.
6. Kernel: physical frame allocator and heap allocator.
7. Screenshot, README, and this document.

## Future work

- **Paging, and the stack guard page it enables.** Without a guard page the
  double-fault handler is installed and correct but never exercised, since
  nothing in the boot path faults on purpose. This also needs the kernel to
  learn where its stack is, which means a new `BootInfo` field.
- **A USB/xHCI HID keyboard driver.** The PS/2 driver works under QEMU but
  not on hardware that exposes its keyboard over USB. That is a new
  subsystem, not an extension of the existing path.
- **A real interactive mode.** Keystrokes currently print one labelled line
  each, and only reach an idle loop as a side effect of the QEMU exit device
  being absent. Inline echo with shift handling would make it deliberate.

## Decisions that changed

Kept because the reasoning is more useful than the conclusion.

**Custom target JSON, dropped at milestone 2.** The original plan was a
hand-written target specification. `x86_64-unknown-none` turned out to
already have the right defaults and ship precompiled core libraries, which
removed both the JSON and the `build-std` step.

**Per-milestone exit codes, dropped at milestone 3.** The plan was for each
milestone to signal progress with its own `isa-debug-exit` code. That would
have required `xtask` to track which milestone was running, for no benefit
once serial output existed and could carry a readable trace.

**Milestone renumbering, at milestone 5.** Framebuffer text turned out to be
independent of the allocator, so it became its own milestone rather than
part of one.

**Hand-rolled allocators, confirmed at milestone 6.** Whether to write both
allocators or pull in `linked_list_allocator` was left open deliberately.
Hand-rolled won: the heap's splitting and coalescing is the most instructive
code in the project, and adding an allocator crate would have undercut the
premise.

**A screenshot rather than a GIF, at milestone 7.** An animated capture
needs an LZW encoder and multi-frame timing. A boot trace appearing line by
line carries no more information than one frame of it does.
