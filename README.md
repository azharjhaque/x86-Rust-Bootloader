# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented from scratch here
rather than delegated to an existing library, milestone by milestone.

## Status

✅ Milestone 2 of 6 complete and verified: the bootloader loads a
separate kernel ELF from the EFI System Partition with a hand-written
ELF64 loader, collects the UEFI memory map and a GOP framebuffer, exits
boot services, and jumps to the kernel — which paints the framebuffer and
reports success itself.

See [docs/design.md](docs/design.md) for the full design, and
[docs/plans/](docs/plans/) for implementation plans per milestone.

## Roadmap

- [x] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
- [x] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
- [ ] 3. Kernel: GDT, IDT, double-fault handler
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
Expected output ends with:

```
PASS: bootloader exited with expected code 33
```

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
