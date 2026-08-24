# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented here.

## Status

🚧 In progress — Milestone 1 of 6 underway: the UEFI bootloader crate
builds; QEMU/OVMF boot verification lands with the `xtask` runner.

See [docs/superpowers/specs/2026-08-23-uefi-bootloader-kernel-design.md](docs/superpowers/specs/2026-08-23-uefi-bootloader-kernel-design.md)
for the full design, and
[docs/superpowers/plans/](docs/superpowers/plans/) for implementation plans
per milestone.

## Roadmap

- [ ] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
- [ ] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
- [ ] 3. Kernel: GDT, IDT, double-fault handler
- [ ] 4. Kernel: PIT timer + PS/2 keyboard interrupts, framebuffer text
- [ ] 5. Kernel: physical frame allocator + heap allocator
- [ ] 6. Polish: docs, screenshots/GIF, write-up

## Prerequisites

- Windows with WSL2 + Ubuntu (`wsl --install -d Ubuntu`)
- Inside WSL/Ubuntu:
  ```bash
  sudo apt install -y build-essential qemu-system-x86 ovmf git curl
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly
  rustup component add rust-src --toolchain nightly
  rustup target add x86_64-unknown-uefi --toolchain nightly
  ```

## Build & run

```bash
cargo xtask run
```

This builds the bootloader, assembles an EFI System Partition image, and
boots it in QEMU under OVMF firmware.
