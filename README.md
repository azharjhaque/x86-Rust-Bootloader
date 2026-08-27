# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented from scratch here
rather than delegated to an existing library, milestone by milestone.

## Status

✅ Milestone 4 of 6 complete and verified: the kernel responds to the
outside world. The two 8259 PICs are remapped off the CPU's own exception
vectors, the PIT drives a 100 Hz timer tick, the 8042 PS/2 controller is
brought up and its keyboard IRQ enabled, and a catch-all handler covers
every remapped PIC vector so no unmasked IRQ can ever land on a non-present
gate. `xtask` proves the keyboard path automatically by injecting a
keystroke through QEMU's monitor, so the whole run needs no human at the
keyboard.

See [docs/design.md](docs/design.md) for the full design, and
[docs/plans/](docs/plans/) for implementation plans per milestone.

## Roadmap

- [x] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
- [x] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
- [x] 3. Kernel: GDT, IDT, double-fault handler
- [x] 4. Kernel: PIT timer + PS/2 keyboard interrupts (framebuffer text
      rendering deferred to Milestone 6 — see
      [docs/plans/milestone-4-interrupts.md](docs/plans/milestone-4-interrupts.md))
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

`xtask` also drives the keyboard test: once QEMU is up it connects to
QEMU's monitor socket and injects `sendkey a` a few times over the following
seconds, which is what lets `cargo xtask run` prove the keyboard IRQ works
with no human present. Running `qemu-system-x86_64` by hand instead (with a
display attached) works the same way, except you type the keystroke
yourself.

The kernel's own trace — everything from here on is the kernel, not the
bootloader — looks like this:

```
=== Rust_BL kernel ===
framebuffer: 1280x800 stride=1280 @ 0x80000000
kernel image: base=0x200000 size=0xf000
GDT + TSS loaded (code selector 0x8)
IDT loaded
PICs remapped to vectors 32-47
PIT programmed at 100 Hz
8042 PS/2 controller initialised
EXCEPTION: breakpoint at 0x200af2 (execution will resume)
selftest: breakpoint handled and execution resumed
framebuffer painted
enabling interrupts
key: 'a'
key: 'a'
timer: 100 ticks received — IRQ0 works
waiting for a keypress...
keyboard: 2 keypress(es) received — IRQ1 works
PASS: bootloader exited with expected code 33
```

Two things worth noting about this trace: the `key: 'a'` lines can appear
*before* the `timer: 100 ticks` line, because `xtask` starts sending
keystrokes on its own schedule as soon as QEMU's monitor socket exists,
independent of how far the kernel has gotten. And `keyboard: 2 keypress(es)`
from a single injected `sendkey a` is expected, not a double-count: QEMU's
`sendkey` produces a make code, a key-repeat, and a break code on real
hardware timing, and the keyboard driver counts every make/repeat while
ignoring the break (bit 7 set) — so one injected keystroke can register as
more than one.

The `PASS` line only appears once both IRQ0 (timer) and IRQ1 (keyboard) have
been proven live. A dead timer hangs until `xtask`'s 60-second timeout
reports a boot hang; a dead keyboard reports itself after a bounded 10-second
wait and exits with the failure code instead.

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
