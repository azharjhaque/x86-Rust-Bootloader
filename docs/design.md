# Rust UEFI Bootloader + Minimal Kernel — Design Spec

## Goal

Build a custom UEFI bootloader and minimal freestanding kernel, written from
scratch in Rust. Primary purpose: learn Rust systems programming and OS
internals, and produce a portfolio/resume-worthy project with a working demo
(boots in QEMU, prints to a framebuffer, handles hardware interrupts).

Explicitly *not* using the `bootloader` crate — the value of this project is
in implementing the boot flow, ELF loading, mode/handoff mechanics, and
interrupt handling ourselves rather than delegating to an existing bootloader
library.

## Scope (MVP — "Approach B")

In scope:
- A UEFI application (`bootloader`) that loads a separate kernel ELF from the
  EFI System Partition, parses and maps it itself, collects boot-time
  information (memory map, framebuffer), exits UEFI boot services, and jumps
  to the kernel entry point.
- A freestanding kernel (`kernel`) that receives the boot info, sets up its
  own GDT and IDT, handles PIT timer and PS/2 keyboard interrupts, and
  renders text to the framebuffer.
- A minimal physical frame allocator and heap allocator in the kernel so
  `alloc` collection types work.
- QEMU + OVMF as the only tested target (no real hardware requirement).
- A `cargo xtask`-based build/run pipeline.

Out of scope for MVP (explicit future work, not to be designed further now):
- Custom virtual memory management / paging beyond what UEFI hands off
- Preemptive multitasking / scheduler
- A real filesystem driver (kernel and bootloader both read only from the ESP
  via UEFI's Simple File System protocol / a FAT structure baked into the
  disk image)
- User-mode processes / syscalls
- A legacy BIOS boot path

## Architecture

Cargo workspace with four crates:

```
Rust_BL/
├── bootloader/     # UEFI application (PE32+), no_std, target x86_64-unknown-uefi
├── kernel/         # freestanding kernel ELF, no_std, custom target JSON
├── boot_info/      # shared #[repr(C)] crate: the handoff struct/ABI
└── xtask/          # build automation: assembles disk image, launches QEMU
```

`boot_info` exists so the bootloader→kernel interface is a real, shared,
versionable Rust type rather than hand-computed memory offsets duplicated in
two places.

## Boot flow

1. OVMF (UEFI firmware, running under QEMU) loads `bootloader.efi` from the
   EFI System Partition and calls its `efi_main`.
2. Bootloader, using the `uefi` crate for boot-services calls:
   - Opens the ESP via the Simple File System protocol and reads
     `kernel.elf` into memory.
   - Parses the ELF program headers itself (no ELF-loading crate) and, for
     each `PT_LOAD` segment, allocates pages via UEFI's `allocate_pages` and
     copies segment data to the segment's `p_vaddr`.
   - Obtains a framebuffer via the Graphics Output Protocol (GOP): address,
     resolution, pixel format, stride.
   - Obtains the UEFI memory map (required immediately before exiting boot
     services, since the map key must match).
3. Bootloader calls `exit_boot_services()`. After this point no further UEFI
   boot-service calls are legal.
4. Bootloader jumps to the kernel's ELF entry point, passing a pointer to a
   `BootInfo` struct (memory map, framebuffer descriptor, kernel image
   base/size).
5. Kernel entry stub (small `core::arch::asm!` block) sets up its own stack,
   then calls `kernel_main(&BootInfo) -> !`.
6. Kernel: writes a startup message to the framebuffer, builds and loads its
   own GDT, builds and loads its own IDT (including a double-fault handler),
   remaps the PIC and unmasks the PIT timer + PS/2 keyboard IRQs, enables
   interrupts, and enters a `hlt` idle loop — echoing keypresses to the
   framebuffer as the interrupt-handling proof point.

## Memory management (MVP scope)

- Physical frame allocator: seeded from the UEFI memory map handed off in
  `BootInfo`; a simple free-list or bump allocator over `EfiConventionalMemory`
  regions is sufficient — no buddy allocator needed at this scope.
- Heap allocator: a small fixed-size kernel heap (hand-rolled bump or
  free-list allocator, registered as the kernel's `#[global_allocator]`) so
  `alloc::vec::Vec`, `Box`, etc. work for later milestones (e.g., keyboard
  input buffering).
- No custom page tables in MVP: the kernel continues using whatever
  identity/mapping UEFI left in place. Building an independent paging setup
  is called out as future work, not designed here.

## Toolchain

- Rust **nightly** with the `rust-src` component (`rustup component add
  rust-src --toolchain nightly`), required for `-Z build-std=core,alloc`.
- `uefi` crate for the bootloader's boot-services bindings.
- Custom target JSON for the kernel (freestanding `x86_64`, kernel code
  model, soft-float where applicable).
- `cargo xtask` crate for build/run orchestration (builds both crates,
  assembles a FAT/GPT disk image with the ESP layout, invokes QEMU) — kept as
  ordinary Rust rather than shell scripts.
- QEMU (`qemu-system-x86_64`) + OVMF firmware for all testing; no physical
  hardware target for this project.
- All assembly is inline `core::arch::asm!` — no external assembler (`nasm`)
  dependency.

## Testing / verification approach

No unit-test framework in the traditional sense (freestanding, no host to run
tests on). Verification is:
- **Boot smoke test**: xtask launches QEMU with the built image; a successful
  boot to the kernel's `hlt` loop without a triple fault is the base
  pass/fail signal.
- **Milestone markers**: the kernel/bootloader write a milestone marker
  (e.g., "GDT loaded", "IDT loaded", "timer interrupt received") to the
  QEMU `isa-debug-exit` device with a distinct exit code, so `xtask` can
  script pass/fail checks per milestone instead of relying on visual
  inspection alone.
- **Visual verification**: framebuffer text output and keyboard echo are
  confirmed by eye in the QEMU window for milestones where that's the
  natural check (text rendering, keypress echo).

## Milestones

These map directly to the implementation plan phases:

1. Workspace scaffold + `xtask` that builds an empty `.efi` and boots it in
   QEMU/OVMF (proves the toolchain end-to-end before any real logic).
2. Bootloader: ELF loader + UEFI memory map + GOP framebuffer acquisition +
   `exit_boot_services` + jump to a kernel stub that writes one pixel.
3. Kernel: GDT + IDT + double-fault handler.
4. Kernel: PIT timer interrupt + PS/2 keyboard interrupt + framebuffer text
   rendering.
5. Kernel: physical frame allocator + heap allocator, prove `alloc` works.
6. Polish: README with build/run instructions, screenshot/GIF of it booting,
   short write-up of what was implemented (this is the artifact that
   actually gets shown on a resume/portfolio).

## Open questions / decisions deferred to implementation

- Hand-rolled bump/free-list heap allocator vs. pulling in
  `linked_list_allocator`: leaning hand-rolled for resume value, final call
  can be made during milestone 5 without affecting earlier milestones.
- Exact custom target JSON contents will be worked out at milestone 1 against
  whatever current nightly requires (these flags shift across Rust
  versions).
