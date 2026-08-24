# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented from scratch here
rather than delegated to an existing library, milestone by milestone.

## Status

✅ Milestone 1 of 6 complete and verified: `cargo xtask run` builds the
bootloader, boots it in QEMU/OVMF, and confirms a clean exit
(`PASS: bootloader exited with expected code 33`).

See [docs/design.md](docs/design.md) for the full design, and
[docs/plans/](docs/plans/) for implementation plans per milestone.

## Roadmap

- [x] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
- [ ] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
- [ ] 3. Kernel: GDT, IDT, double-fault handler
- [ ] 4. Kernel: PIT timer + PS/2 keyboard interrupts, framebuffer text
- [ ] 5. Kernel: physical frame allocator + heap allocator
- [ ] 6. Polish: docs, screenshots/GIF, write-up

## Repository layout

```
├── bootloader/   # UEFI application (PE32+), no_std, x86_64-unknown-uefi
├── xtask/        # build automation: stages the ESP, launches QEMU
└── docs/         # design spec and per-milestone implementation plans
```

(`kernel/` and `boot_info/` arrive in Milestone 2.)

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
installs the `rust-src` component and the `x86_64-unknown-uefi` target the
first time you run `cargo` here — no separate `rustup component add` /
`rustup target add` steps needed.

## Build & run

```bash
git clone <this-repo-url>
cd Rust_BL
cargo xtask run
```

This builds the bootloader, stages an EFI System Partition directory, and
boots it in QEMU under OVMF firmware.
