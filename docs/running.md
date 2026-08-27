# Building, running, and testing Rust_BL

Operational detail split out of the README. For what the project is and how
it works, start there.

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

This builds the bootloader and kernel, stages an EFI System Partition
directory (the bootloader's `.efi` under `EFI/BOOT/`, plus `kernel.elf` at
the ESP root), and boots it in QEMU under OVMF firmware. The bootloader
loads and hands off to the kernel, which paints the framebuffer blue and
mirrors its boot trace to both COM1 and the screen. A normal run keeps the
QEMU display off (`-display none`), but xtask captures the 1280x800 GOP
surface through QEMU's monitor and verifies that it contains text as well as
the expected success exit code.

`xtask` also drives the keyboard test: once QEMU is up it connects to QEMU's
monitor socket and injects `sendkey a` repeatedly, every 500ms, for as long
as QEMU keeps running (or until a generous backstop expires) — which is what
lets `cargo xtask run` prove the keyboard IRQ works with no human present,
on hosts slow enough that boot alone eats several seconds. Running
`qemu-system-x86_64` by hand instead (with a display attached) works the
same way, except you type the keystroke yourself.

## The boot trace

The kernel's own trace — everything from here on is the kernel, not the
bootloader — looks like this:

```
=== Rust_BL kernel ===
framebuffer: 1280x800 stride=1280 @ 0x80000000
kernel image: base=0x200000 size=0x19000
GDT + TSS loaded (code selector 0x8)
IDT loaded
PICs remapped to vectors 32-47
PIT programmed at 100 Hz
8042 PS/2 controller initialised
EXCEPTION: breakpoint at 0x201322 (execution will resume)
selftest: breakpoint handled and execution resumed
framebuffer painted
frame allocator: 53195 usable frames (207 MiB)
frame allocator: selftest passed
heap: 1024 KiB @ 0x100000 (256 of 53195 frames in use)
alloc: Box/Vec/String OK, heap balanced
enabling interrupts
key: 'a'
timer: 100 ticks received - IRQ0 works
waiting for a keypress...
keyboard: 1 key event(s) received - IRQ1 works
PASS: bootloader exited with expected code 33, and the screen has text
```

Two things worth noting about this trace: the `key: 'a'` lines can appear
*before* the `timer: 100 ticks` line, because `xtask` starts sending
keystrokes on its own schedule as soon as QEMU's monitor socket exists,
independent of how far the kernel has gotten. And the exact
`keyboard: N key event(s)` count is timing-dependent, not a fixed number:
`xtask` injects `sendkey a` repeatedly, and the kernel reports whatever has
accumulated by the time it first checks — so seeing more than one event from
what looks like "one keystroke" just means more than one injection had
already landed.

The `PASS` line only appears once both IRQ0 (timer) and IRQ1 (keyboard) have
been proven live. A dead timer hangs until `xtask`'s 60-second timeout
reports a boot hang; a dead keyboard reports itself after a bounded
10-second wait and exits with the failure code instead.

The double-fault handler from Milestone 3 is still installed as the safety
net — vector 8 in the IDT still runs on its own IST stack — but it no longer
appears in this trace. Milestone 3 could afford to provoke one deliberately
on every boot, as the kernel had nothing left to do afterward; the kernel
now has to keep running to service IRQ0 and IRQ1, so nothing in the current
boot path triggers a fault on purpose any more.

An earlier version of the README predicted that Milestone 6 would add a
stack guard page and make a genuine stack-overflow double fault happen
naturally. It did not: a guard page means unmapping a page, which means
managing page tables, and [design.md](design.md) deliberately keeps custom
paging out of the MVP — the kernel still runs on the identity mapping UEFI
leaves behind. Milestone 6 allocates *physical* memory only. Making the
double-fault handler testable again therefore stays future work, alongside
paging.

## Testing the failure path

```bash
cargo xtask test
```

`cargo xtask run` only ever exercises the happy path, which leaves every
validation branch in the ELF loader — magic, class, endianness, machine,
bounds and overflow checks, entry-point range — untested. All of it could be
deleted and `run` would still report PASS.

`cargo xtask test` closes that gap: it corrupts the staged kernel's ELF
magic, boots it, and checks that the bootloader *rejects* the image rather
than jumping into it. The distinction matters because the loader validates
before `ExitBootServices`: caught there it is a logged error and a clean
exit, missed it is a triple fault with no logger left to report anything.
The original image is restored afterward. Expected output ends with:

```
PASS: bootloader rejected the corrupted image (exit 35) instead of jumping into it
```

## Host unit tests

```bash
cargo test -p boot_info -p xtask
```

These run natively, not in QEMU. `boot_info` covers the memory-region
arithmetic that feeds the frame allocator (alignment clamping, non-usable
regions, sub-page runs, the frame at physical zero). `xtask` covers
screen-capture validation (PPM parsing, dimension and maxval checks,
truncated payloads).

## Regenerating the screenshot

```bash
cargo xtask run                        # stage the ESP first
python3 tools/capture_screenshot.py    # writes docs/images/boot.png
```

Two details in that script are load-bearing and easy to get wrong if you
reimplement it:

**It omits `-device isa-debug-exit`,** which `xtask` always passes. With
that device, `qemu_exit::exit` shuts QEMU down with a verdict exit code.
Without it, the port write goes nowhere and `exit` falls through into its
own `hlt` loop — with interrupts still enabled. IRQ1 keeps firing, so
keystrokes keep reaching the framebuffer. That fallback is what makes an
interactive screenshot possible, and it is the idle echo loop
[design.md](design.md)'s boot flow describes.

**It copies `OVMF_VARS.fd` before booting.** That pflash drive is opened
read-write. Sharing one file between two concurrent QEMU instances — this
script and a manual `qemu-system-x86_64` in another terminal, say —
corrupts the firmware's boot entries: the guest never reaches the
bootloader, and the capture comes back as a 640x480 firmware-mode screen
with an empty serial log.

`tools/ppm_to_png.py` does the PPM→PNG conversion on its own if you have a
capture already. Both scripts are one-off authoring tools, like
`tools/generate_font.py`: their output is committed, and building or testing
the project never runs them.
