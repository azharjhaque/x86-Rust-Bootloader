# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented from scratch here
rather than delegated to an existing library, milestone by milestone.

## Status

✅ Milestone 3 of 6 complete and verified: the kernel now reports over its
own COM1 serial driver, builds and loads its own GDT and TSS, installs a
256-entry IDT, handles a breakpoint exception, and deliberately provokes and
catches a double fault on a dedicated IST stack instead of letting the CPU
triple-fault and reset the machine.

See [docs/design.md](docs/design.md) for the full design, and
[docs/plans/](docs/plans/) for implementation plans per milestone.

## Roadmap

- [x] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
- [x] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
- [x] 3. Kernel: GDT, IDT, double-fault handler
- [ ] 4. Kernel: PIT timer + PS/2 keyboard interrupts, framebuffer text
- [ ] 5. Kernel: physical frame allocator + heap allocator
- [ ] 6. Polish: docs, screenshots/GIF, write-up

## Repository layout

```
├── bootloader/   # UEFI application (PE32+), no_std, x86_64-unknown-uefi
├── boot_info/    # shared #[repr(C)] handoff ABI between the two
├── kernel/       # freestanding kernel ELF, no_std, x86_64-unknown-none
├── xtask/        # build automation: stages the ESP, launches QEMU
└── docs/         # design spec and per-milestone implementation plans
```

## Prerequisites

The real constraint is a Debian/Ubuntu-based Linux system: `xtask` hardcodes
the Debian/Ubuntu `ovmf` package's OVMF firmware paths
(`/usr/share/OVMF/OVMF_CODE_4M.fd` and `OVMF_VARS_4M.fd`). Any Linux with
QEMU works in principle — these are just two constants near the top of
`xtask/src/main.rs`, easy to adjust for other distros (Fedora ships OVMF
under `/usr/share/edk2/ovmf/`, Arch under `/usr/share/edk2-ovmf/x64/`).
Windows is not required; WSL2 + Ubuntu is simply the easiest way to get a
Debian/Ubuntu environment if you're on Windows (`wsl --install -d Ubuntu`).

Inside your Debian/Ubuntu environment (WSL2 or otherwise):

```bash
sudo apt install -y build-essential qemu-system-x86 ovmf git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly
```

`rust-toolchain.toml` in this repo pins the exact nightly and automatically
installs the `rust-src` component and both targets this workspace needs —
`x86_64-unknown-uefi` for the bootloader and `x86_64-unknown-none` for the
kernel — the first time you run `cargo` here. No separate `rustup component
add` / `rustup target add` steps needed.

## Build & run

```bash
git clone <this-repo-url>
cd Rust_BL
cargo xtask run
```

This builds the bootloader and the kernel, stages an EFI System Partition
directory (the bootloader's `.efi` under `EFI/BOOT/`, plus `kernel.elf` at
the ESP root), and boots it in QEMU under OVMF firmware. The bootloader
loads and hands off to the kernel, which paints the framebuffer blue and
reports success itself. A normal run keeps the QEMU display off
(`-display none`) so the kernel's blue screen isn't visible — the pass/fail
signal comes from the QEMU exit code and the serial log, not the display.
Once the kernel has taken over from the UEFI console, it reports its own
progress on the same serial console via a COM1 driver, so its output now
appears right after the bootloader's handing off to kernel line.

The kernel's own trace — everything from here on is the kernel, not the
bootloader — looks like this:

```
=== Rust_BL kernel ===
framebuffer: 1280x800 stride=1280 @ 0x80000000
kernel image: base=0x200000 size=0xd000
GDT + TSS loaded (code selector 0x8)
double-fault IST index: 0
IDT loaded
EXCEPTION: breakpoint at 0x200797 (execution will resume)
resumed after breakpoint
framebuffer painted
kernel reached the end of milestone 3 setup

about to raise #UD with no vector-6 handler installed;
the CPU should escalate it to a double fault...
EXCEPTION: double fault
  faulting instruction: 0x200928
  interrupted stack:    0xdfa7e60
  handler stack:        0x20be98
  fault stack spans:    0x208010..0x20c010
  handler is running on the IST stack — the machine did not reset
PASS: bootloader exited with expected code 33
```

The `EXCEPTION: double fault` near the end is expected, not a crash: the
kernel deliberately executes `ud2` (an invalid opcode) with no handler
registered for vector 6, so the CPU escalates the fault it can't deliver
into a double fault (vector 8). That double fault is caught by a handler
running on its own dedicated IST stack — proven above by the handler stack
address falling inside the reported fault-stack range — rather than the
alternative, which is the CPU resetting the machine outright (a triple
fault, invisible to this log). The `PASS` line only appears because the
double-fault handler confirms both that this was the fault it expected and
that it ran on the IST stack; any other unhandled exception, or a broken
IST switch, reports failure instead.

## Testing the failure path

```bash
cargo xtask test
```

`cargo xtask run` only ever exercises the happy path, which leaves every
validation branch in the ELF loader — magic, class, endianness, machine,
bounds and overflow checks, entry-point range — untested. All of it could be
deleted and `run` would still report PASS.

`cargo xtask test` closes that gap: it corrupts the staged kernel's ELF magic,
boots it, and checks that the bootloader *rejects* the image rather than
jumping into it. The distinction matters because the loader validates before
`ExitBootServices`: caught there it is a logged error and a clean exit, missed
it is a triple fault with no logger left to report anything. The original
image is restored afterward. Expected output ends with:

```
PASS: bootloader rejected the corrupted image (exit 35) instead of jumping into it
```
