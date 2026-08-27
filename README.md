# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal x86_64 kernel, written in Rust
with no OS-provided runtime. No `bootloader` crate, no borrowed kernel, no
allocator crate: the boot flow, ELF loading, interrupt handling, and memory
management are all implemented here rather than delegated to a library.

![The kernel booted in QEMU, showing its allocator trace and live keyboard input](docs/images/boot.png)

That screenshot is the kernel's own framebuffer console — glyphs blitted
pixel by pixel from an embedded 8×16 bitmap font onto a UEFI GOP surface,
with no firmware text services involved (they are gone by then). The
`key: 'h'` lines are live PS/2 keystrokes arriving on IRQ1 and being echoed
as they are typed.

## What it does

Boots under OVMF, loads a separate kernel ELF off the EFI System Partition,
parses and maps it by hand, exits UEFI boot services, and jumps to a
freestanding kernel that sets up its own GDT, IDT, and interrupt
controllers, then manages its own physical memory with a frame allocator and
a coalescing heap.

## The parts worth reading

Most of this project's value is in the places where the hardware or the
firmware contract refuses to cooperate. A few of those:

**Validating the kernel ELF *before* the point of no return.**
[`bootloader/src/elf.rs`](bootloader/src/elf.rs) checks magic, class,
endianness, machine, segment bounds, and entry-point range — all before
`ExitBootServices`. The ordering is the whole point: caught there, a bad
image is a logged error and a clean exit; missed, it is a triple fault with
no logger left alive to report anything. `cargo xtask test` deliberately
corrupts the image to prove that path still works.

**The stack has to arrive misaligned.** The SysV ABI guarantees a function
sees `rsp % 16 == 8` at entry, because an ordinary `call` pushed an 8-byte
return address. The kernel is reached by `jmp`, which pushes nothing, so
[`handoff.rs`](bootloader/src/handoff.rs) biases the stack pointer by 8 by
hand. Skip it and every stack slot LLVM believes is 16-byte aligned is off
by eight — the first aligned SSE spill faults, with no handler installed to
say so.

**Interrupts must be off across the handoff.** `ExitBootServices` leaves
`IF` set and `IDTR` still pointing at firmware memory that is about to be
handed back to the allocator. A single interrupt in that window reads a
descriptor out of memory that may no longer hold one, so the bootloader
executes `cli` before the jump — a precondition living in a different binary
from the code that depends on it.

**`-z relro` breaks the loader, subtly.** The linker's default carves a tiny
`.got` into its own non-page-aligned `PT_LOAD` segment, landing on the same
page as `.rodata`. Since the loader allocates each segment's pages
independently, the second claim on that shared page fails outright — before
the kernel runs at all. [`kernel/build.rs`](kernel/build.rs) passes
`-znorelro` to fold `.got` into the already-aligned `.data`.

**Free lists that live inside the free memory.** Neither allocator keeps a
side table. A free physical frame stores its "next" pointer in its own first
eight bytes ([`kernel/src/frame.rs`](kernel/src/frame.rs)); a free heap block
stores its size and link in its own header
([`kernel/src/heap.rs`](kernel/src/heap.rs)). Bookkeeping costs zero bytes of
separate storage, and the heap coalesces with both neighbours on free, which
is what keeps repeated `Vec` growth from shredding it into unusable slivers.

**One critical-section primitive, deliberately not `nomem`.**
[`interrupts::without_interrupts`](kernel/src/interrupts.rs) saves and
restores `IF` rather than blindly re-enabling it, because it is called from
inside handlers where interrupts are already off. It also omits
`options(nomem)` on purpose: that would let LLVM reorder loads and stores
across the boundary, so a store from inside the critical section could sink
past the `sti`. Omitting it makes each block a compiler barrier.

## Quick start

Requires a Debian/Ubuntu system (WSL2 works) with QEMU and OVMF:

```bash
sudo apt install -y build-essential qemu-system-x86 ovmf git curl
```

Build, boot, and run the automated checks:

```bash
cargo xtask run
```

That runs headless and prints the boot trace to your terminal. To watch it
on an actual screen and type into it yourself, stage the image first and
then launch QEMU with a display attached:

```bash
cargo xtask run
qemu-system-x86_64 \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=target/OVMF_VARS.fd \
  -drive format=raw,file=fat:rw:target/esp \
  -m 256M
```

A QEMU window opens, the kernel paints it blue, and the boot trace appears
in the framebuffer console. Anything you type shows up as it arrives on
IRQ1. Note the missing `-device isa-debug-exit` here: without it the kernel's
exit path falls through into its idle loop instead of shutting QEMU down,
which is what leaves the window open to type into.

`rust-toolchain.toml` pins the nightly and installs both targets on first
use. Full prerequisites, the annotated boot trace, and the failure-path test
are in **[docs/running.md](docs/running.md)**.

## Repository layout

```
├── bootloader/   # UEFI application (PE32+), no_std, x86_64-unknown-uefi
├── boot_info/    # shared #[repr(C)] handoff ABI between the two
├── kernel/       # freestanding kernel ELF, no_std, x86_64-unknown-none
├── xtask/        # build automation: stages the ESP, launches QEMU
├── tools/        # one-off authoring scripts (font, screenshot)
└── docs/         # design spec and running guide
```

## How it is tested

There is no test framework in a freestanding kernel, so verification is
layered instead:

| Layer | Command | What it proves |
|---|---|---|
| Host unit tests | `cargo test -p boot_info -p xtask` | Memory-region arithmetic and screen-capture validation, run natively |
| Boot smoke test | `cargo xtask run` | Boots to the idle loop, exit code 33, screen contains real glyphs, IRQ0 and IRQ1 both live |
| Failure path | `cargo xtask test` | A corrupted ELF is *rejected* rather than jumped into (exit 35) |

The kernel does its own assertions and exits with a verdict code, so a
broken invariant fails the build rather than printing a warning nobody
reads. Keyboard input is injected through QEMU's monitor, so the interrupt
tests need no human.

## Documentation

- [docs/design.md](docs/design.md) — the design: scope, architecture, boot
  flow, memory management, and the decisions that changed along the way
- [docs/running.md](docs/running.md) — prerequisites, build/run, annotated
  trace, failure-path test

## License

See [LICENSE](LICENSE).
