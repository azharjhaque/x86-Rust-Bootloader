# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented from scratch here
rather than delegated to an existing library, milestone by milestone.

## Status

Milestone 5 of 7 is complete and verified: the kernel now mirrors its boot
trace to the framebuffer as well as COM1. The console draws glyphs directly
onto the blue GOP surface, while xtask captures the kernel's 1280x800 screen
and rejects a flat image. The existing 8259 PIC, PIT, and PS/2 keyboard
checks remain automated, so the full boot trace is visible on machines
without a serial port as well as in QEMU.

See [docs/design.md](docs/design.md) for the full design, and
[docs/plans/](docs/plans/) for implementation plans per milestone.

## Roadmap

- [x] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
- [x] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
- [x] 3. Kernel: GDT, IDT, double-fault handler
- [x] 4. Kernel: PIT timer + PS/2 keyboard interrupts
- [x] 5. Kernel: framebuffer text rendering and serial fan-out
- [ ] 6. Kernel: physical frame allocator + heap allocator
- [ ] 7. Polish: docs, screenshots/GIF, write-up

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

This builds the bootloader and kernel, stages an EFI System Partition directory
(the bootloader's .efi under EFI/BOOT/, plus kernel.elf at the ESP root), and
boots it in QEMU under OVMF firmware. The bootloader loads and hands off to
the kernel, which paints the framebuffer blue and mirrors its boot trace to
both COM1 and the screen. A normal run keeps the QEMU display off
(-display none), but xtask captures the 1280x800 GOP surface through QEMU's
monitor and verifies that it contains text as well as the expected success
exit code and serial trace.

`xtask` also drives the keyboard test: once QEMU is up it connects to
QEMU's monitor socket and injects `sendkey a` repeatedly, every 500ms, for
as long as QEMU keeps running (or until a generous backstop expires) —
which is what lets `cargo xtask run` prove the keyboard IRQ works with no
human present, on hosts slow enough that boot alone eats several seconds.
Running `qemu-system-x86_64` by hand instead (with a display attached)
works the same way, except you type the keystroke yourself.

The kernel's own trace — everything from here on is the kernel, not the
bootloader — looks like this:

```
=== Rust_BL kernel ===
framebuffer: 1280x800 stride=1280 @ 0x80000000
kernel image: base=0x200000 size=0x11000
GDT + TSS loaded (code selector 0x8)
IDT loaded
PICs remapped to vectors 32-47
PIT programmed at 100 Hz
8042 PS/2 controller initialised
EXCEPTION: breakpoint at 0x200cc2 (execution will resume)
selftest: breakpoint handled and execution resumed
framebuffer painted
enabling interrupts
key: 'a'
timer: 100 ticks received — IRQ0 works
waiting for a keypress...
keyboard: 1 key event(s) received — IRQ1 works
PASS: bootloader exited with expected code 33, and the screen has text
```

Two things worth noting about this trace: the `key: 'a'` lines can appear
*before* the `timer: 100 ticks` line, because `xtask` starts sending
keystrokes on its own schedule as soon as QEMU's monitor socket exists,
independent of how far the kernel has gotten. And the exact
`keyboard: N key event(s)` count is timing-dependent, not a fixed number:
`xtask` injects `sendkey a` repeatedly (every 500ms, for as long as QEMU
keeps running), and the kernel reports whatever has accumulated by the time
it first checks — so seeing more than one event from what looks like "one
keystroke" just means more than one injection had already landed.

The `PASS` line only appears once both IRQ0 (timer) and IRQ1 (keyboard) have
been proven live. A dead timer hangs until `xtask`'s 60-second timeout
reports a boot hang; a dead keyboard reports itself after a bounded 10-second
wait and exits with the failure code instead.

The double-fault handler from Milestone 3 is still installed as the
safety net — vector 8 in the IDT still runs on its own IST stack — but it
no longer appears in this trace. Milestone 3 could afford to provoke one
deliberately on every boot, as the kernel had nothing left to do
afterward; this milestone's kernel has to keep running to service IRQ0 and
IRQ1, so nothing in the current boot path triggers a fault on purpose any
more. Milestone 6's stack guard page will make a genuine stack-overflow
double fault happen naturally, at which point this handler becomes
testable again rather than just present.

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
