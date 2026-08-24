# Milestone 1: Toolchain Bootstrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a WSL2/Ubuntu dev environment and a Cargo workspace that
builds a minimal `#![no_std]` UEFI application and boots it in QEMU/OVMF,
proving the entire toolchain (Rust → UEFI target → QEMU → OVMF firmware)
works end-to-end, with a scripted pass/fail signal — before any real
bootloader logic is written.

**Architecture:** A WSL2 Ubuntu environment provides QEMU, OVMF firmware, and
the Rust toolchain via `apt`/`rustup`. A two-crate Cargo workspace
(`bootloader`, `xtask`) lives in the repo; `bootloader` is a `#![no_std]`
UEFI application using the `uefi` crate, and `xtask` is a plain Rust binary
that builds it, assembles a FAT-formatted EFI System Partition directory, and
launches QEMU with OVMF firmware pointed at that directory. The bootloader
signals success via QEMU's `isa-debug-exit` device so the boot result is a
checkable process exit code, not just a visual inspection.

**Tech Stack:** Rust nightly (`rust-src` component, `x86_64-unknown-uefi`
target — this target ships precompiled `no_std` support since Rust 1.67, so
no `-Z build-std` is needed for this crate), `uefi` crate 0.39.0, WSL2 +
Ubuntu, QEMU (`qemu-system-x86_64`) + OVMF firmware via `apt`.

**Spec:** [docs/superpowers/specs/2026-08-23-uefi-bootloader-kernel-design.md](../specs/2026-08-23-uefi-bootloader-kernel-design.md)

## Global Constraints

- Rust **nightly** toolchain with `rust-src` component (needed later for the
  kernel crate; pinned project-wide now for consistency).
- No `bootloader` crate dependency — boot flow is implemented by hand.
- QEMU + OVMF is the only tested target; no real-hardware requirement.
- All assembly is inline `core::arch::asm!` — no external assembler.
- Build/run orchestration lives in a `cargo xtask` crate, not shell scripts.
- The repo's working copy moves to the WSL native filesystem
  (`~/projects/Rust_BL`) for build performance; the existing Windows/OneDrive
  clone (`/mnt/c/Users/coolg/OneDrive/Desktop/Rust_BL`) is not deleted, but
  is no longer the active working copy after Task 1.

---

### Task 1: WSL2 environment + toolchain bootstrap

**Files:** none (environment setup only; no repo files change in this task).

**Interfaces:**
- Produces: a working `wsl -d Ubuntu -- bash -lc "<cmd>"` execution path from
  the Windows host, a `~/projects/Rust_BL` git clone inside WSL with the
  existing spec commit, and confirmed `cargo`, `rustc`, `qemu-system-x86_64`,
  and OVMF firmware files available inside that WSL environment. All later
  tasks assume these exist.

- [ ] **Step 1: Check whether WSL2 with an Ubuntu distro is already installed**

Run (from a Windows PowerShell or Command Prompt window, not the sandboxed
tool):

```powershell
wsl -l -v
```

Expected: a list including a distro named `Ubuntu` (or `Ubuntu-22.04` /
`Ubuntu-24.04`) with `VERSION` column showing `2`. If you see this, skip to
Step 3.

- [ ] **Step 2: Install WSL2 + Ubuntu if missing (interactive — must be run by hand, not scripted)**

In an elevated PowerShell window:

```powershell
wsl --install -d Ubuntu
```

This downloads and installs WSL2 and Ubuntu, then reboots if required. On
first launch, Ubuntu will prompt interactively for a UNIX username and
password — this step cannot be automated by an agent and must be completed
by you in a real terminal window. Once you've created the user and see a
shell prompt inside Ubuntu, this step is done.

- [ ] **Step 3: Install system packages inside WSL/Ubuntu**

```bash
wsl -d Ubuntu -- bash -lc "sudo apt update && sudo apt install -y build-essential qemu-system-x86 ovmf git curl"
```

This prompts for the WSL user's `sudo` password interactively the first
time — run it in a real terminal if it's not already cached.

- [ ] **Step 4: Verify QEMU and OVMF are present**

```bash
wsl -d Ubuntu -- bash -lc "qemu-system-x86_64 --version && ls /usr/share/OVMF/OVMF_CODE.fd /usr/share/OVMF/OVMF_VARS.fd"
```

Expected: a QEMU version string, and both `OVMF_CODE.fd` and `OVMF_VARS.fd`
listed with no "No such file" errors.

- [ ] **Step 5: Install Rust nightly via rustup inside WSL**

```bash
wsl -d Ubuntu -- bash -lc "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly"
```

- [ ] **Step 6: Add the `rust-src` component and the UEFI target**

```bash
wsl -d Ubuntu -- bash -lc "source \$HOME/.cargo/env && rustup component add rust-src --toolchain nightly && rustup target add x86_64-unknown-uefi --toolchain nightly"
```

- [ ] **Step 7: Verify the Rust toolchain**

```bash
wsl -d Ubuntu -- bash -lc "source \$HOME/.cargo/env && rustc --version && rustup target list --installed | grep uefi"
```

Expected: a `rustc ... nightly` version string, and `x86_64-unknown-uefi` in
the installed-targets list.

- [ ] **Step 8: Configure git identity inside WSL (separate config store from Windows git)**

WSL's git is a separate installation from Git for Windows, so it needs its
own identity even if you already configured one on the Windows side. Run
this yourself in a WSL terminal (do not have an agent set git config on your
behalf):

```bash
git config --global user.name "Your Name"
git config --global user.email "mobashsharh@gmail.com"
```

- [ ] **Step 9: Clone the existing repo into the WSL native filesystem**

```bash
wsl -d Ubuntu -- bash -lc "mkdir -p ~/projects && git clone /mnt/c/Users/coolg/OneDrive/Desktop/Rust_BL ~/projects/Rust_BL"
```

- [ ] **Step 10: Verify the clone has the existing spec commit**

```bash
wsl -d Ubuntu -- bash -lc "cd ~/projects/Rust_BL && git log --oneline"
```

Expected: shows the `Add design spec for UEFI bootloader + minimal kernel
project` commit. From this point on, `~/projects/Rust_BL` inside WSL is the
active working copy; all remaining tasks in this plan operate there.

---

### Task 2: Cargo workspace + bootloader crate skeleton

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `bootloader/Cargo.toml`
- Create: `bootloader/src/main.rs`
- Create: `README.md`

**Interfaces:**
- Consumes: the WSL environment and cloned repo from Task 1.
- Produces: a `bootloader` crate that builds to
  `target/x86_64-unknown-uefi/debug/bootloader.efi`. Task 3 consumes this
  exact path.

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["bootloader"]
```

(`xtask` is added to `members` in Task 3; `kernel` and `boot_info` are added
in the Milestone 2 plan, not here.)

- [ ] **Step 2: Pin the toolchain with `rust-toolchain.toml`**

```toml
[toolchain]
channel = "nightly"
components = ["rust-src"]
targets = ["x86_64-unknown-uefi"]
```

This means anyone (including CI later) who runs `cargo` inside this repo
automatically gets the right toolchain, components, and target installed by
rustup without manual setup.

- [ ] **Step 3: Create `.gitignore`**

```
/target/
```

- [ ] **Step 4: Create the `bootloader` crate's `Cargo.toml`**

```toml
[package]
name = "bootloader"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
uefi = { version = "0.39.0", features = ["panic_handler"] }
log = "0.4"
```

The `panic_handler` feature makes the `uefi` crate supply a `#[panic_handler]`
that logs the panic and hangs — we don't need to write our own for now.

- [ ] **Step 5: Write the minimal bootloader entry point**

`bootloader/src/main.rs`:

```rust
#![no_main]
#![no_std]

use uefi::boot;
use uefi::prelude::*;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    log::info!("Rust_BL bootloader: milestone 1 toolchain smoke test");

    boot::stall(2_000_000); // 2 seconds, so the log line is visible on screen

    Status::SUCCESS
}
```

- [ ] **Step 6: Build the bootloader for the UEFI target**

```bash
wsl -d Ubuntu -- bash -lc "cd ~/projects/Rust_BL && source \$HOME/.cargo/env && cargo build -p bootloader --target x86_64-unknown-uefi"
```

Expected: build succeeds with no errors.

- [ ] **Step 7: Verify the `.efi` binary was produced**

```bash
wsl -d Ubuntu -- bash -lc "cd ~/projects/Rust_BL && ls -la target/x86_64-unknown-uefi/debug/bootloader.efi"
```

Expected: the file exists and is a non-trivial size (tens to low hundreds of
KB, not 0 bytes).

- [ ] **Step 8: Write the initial `README.md`**

```markdown
# Rust_BL — A UEFI Bootloader + Kernel, Written From Scratch in Rust

A from-scratch UEFI bootloader and minimal kernel for x86_64, written in
Rust with no OS-provided runtime. Built to learn Rust systems programming
and OS internals — no `bootloader` crate, no borrowed kernel: the boot flow,
ELF loading, and interrupt handling are all implemented here.

## Status

🚧 In progress — Milestone 1 of 6 complete (toolchain bootstrap).

See [docs/superpowers/specs/2026-08-23-uefi-bootloader-kernel-design.md](docs/superpowers/specs/2026-08-23-uefi-bootloader-kernel-design.md)
for the full design, and
[docs/superpowers/plans/](docs/superpowers/plans/) for implementation plans
per milestone.

## Roadmap

- [x] 1. Toolchain bootstrap — empty UEFI app boots in QEMU/OVMF
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
```

- [ ] **Step 9: Commit**

```bash
wsl -d Ubuntu -- bash -lc "cd ~/projects/Rust_BL && git add Cargo.toml rust-toolchain.toml .gitignore bootloader README.md && git commit -m 'Add workspace scaffold and minimal UEFI bootloader skeleton'"
```

---

### Task 3: `xtask` build/run automation + QEMU boot verification

**Files:**
- Modify: `Cargo.toml` (add `xtask` to workspace members)
- Modify: `bootloader/src/main.rs` (call the exit helper instead of returning
  `Status::SUCCESS` directly)
- Create: `bootloader/src/qemu_exit.rs`
- Create: `xtask/Cargo.toml`
- Create: `xtask/src/main.rs`
- Create: `.cargo/config.toml`
- Modify: `README.md` (mark Milestone 1 verified, note the `PASS`/`FAIL`
  output)

**Interfaces:**
- Consumes: `target/x86_64-unknown-uefi/debug/bootloader.efi` (from Task 2),
  `/usr/share/OVMF/OVMF_CODE.fd` and `/usr/share/OVMF/OVMF_VARS.fd` (from
  Task 1).
- Produces: the `cargo xtask run` command, which later milestones' plans
  will extend rather than replace. `qemu_exit::exit(QemuExitCode) -> !` is
  the function later milestone code should call to report a scripted
  pass/fail signal from within the bootloader or kernel.

- [ ] **Step 1: Add the QEMU exit-signaling helper to the bootloader**

`bootloader/src/qemu_exit.rs`:

```rust
use core::arch::asm;

const ISA_DEBUG_EXIT_PORT: u16 = 0xf4;

#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Writes to the `isa-debug-exit` device, which shuts QEMU down with
/// process exit code `(value << 1) | 1`. Only meaningful when QEMU is
/// launched with `-device isa-debug-exit,iobase=0xf4,iosize=0x04` (which
/// `xtask` always does); this is a no-op on real hardware, which is fine
/// since this project only targets QEMU.
pub fn exit(code: QemuExitCode) -> ! {
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") ISA_DEBUG_EXIT_PORT,
            in("eax") code as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
    loop {
        unsafe { asm!("hlt", options(nomem, nostack)) }
    }
}
```

- [ ] **Step 2: Wire the exit helper into `main.rs`**

Replace the body of `bootloader/src/main.rs` with:

```rust
#![no_main]
#![no_std]

use uefi::boot;
use uefi::prelude::*;

mod qemu_exit;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    log::info!("Rust_BL bootloader: milestone 1 toolchain smoke test");

    boot::stall(2_000_000); // 2 seconds, so the log line is visible on screen

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}
```

- [ ] **Step 3: Add `xtask` to the workspace**

`Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["bootloader", "xtask"]
```

- [ ] **Step 4: Create the `xtask` crate manifest**

`xtask/Cargo.toml`:

```toml
[package]
name = "xtask"
version = "0.1.0"
edition = "2021"
publish = false
```

No dependencies — `xtask` only shells out to `cargo`, filesystem calls, and
`qemu-system-x86_64` via `std::process::Command`.

- [ ] **Step 5: Write `xtask`'s build/stage/run logic**

`xtask/src/main.rs`:

```rust
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE.fd";
const OVMF_VARS_SRC: &str = "/usr/share/OVMF/OVMF_VARS.fd";
// Matches qemu_exit::QemuExitCode::Success (0x10): QEMU maps a written
// value `v` to process exit code `(v << 1) | 1`.
const EXPECTED_EXIT_CODE: i32 = 33;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => run(),
        _ => {
            eprintln!("Usage: cargo xtask run");
            ExitCode::FAILURE
        }
    }
}

fn run() -> ExitCode {
    build_bootloader();
    let esp_dir = stage_esp();
    let vars_copy = copy_ovmf_vars();

    let status = Command::new("qemu-system-x86_64")
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"))
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars_copy.display()))
        .arg("-drive")
        .arg(format!("format=raw,file=fat:rw:{}", esp_dir.display()))
        .arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04")
        .arg("-m")
        .arg("256M")
        .status()
        .expect(
            "failed to launch qemu-system-x86_64 (is it installed? try: sudo apt install qemu-system-x86)",
        );

    match status.code() {
        Some(EXPECTED_EXIT_CODE) => {
            println!("PASS: bootloader exited with expected code {EXPECTED_EXIT_CODE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("FAIL: expected exit code {EXPECTED_EXIT_CODE}, got {other}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("FAIL: qemu-system-x86_64 exited via signal, no exit code");
            ExitCode::FAILURE
        }
    }
}

fn build_bootloader() {
    let status = Command::new("cargo")
        .args(["build", "-p", "bootloader", "--target", "x86_64-unknown-uefi"])
        .status()
        .expect("failed to run cargo build");
    assert!(status.success(), "cargo build failed");
}

fn stage_esp() -> PathBuf {
    let esp_boot_dir = Path::new("target/esp/EFI/BOOT");
    fs::create_dir_all(esp_boot_dir).expect("failed to create ESP directory structure");
    fs::copy(
        "target/x86_64-unknown-uefi/debug/bootloader.efi",
        esp_boot_dir.join("BOOTX64.EFI"),
    )
    .expect("failed to copy bootloader.efi into the ESP (did the build produce it?)");
    Path::new("target/esp").to_path_buf()
}

fn copy_ovmf_vars() -> PathBuf {
    let dest = Path::new("target/OVMF_VARS.fd");
    fs::copy(OVMF_VARS_SRC, dest).expect(
        "failed to copy OVMF_VARS.fd (is the 'ovmf' package installed? try: sudo apt install ovmf)",
    );
    dest.to_path_buf()
}
```

QEMU is given its own writable copy of `OVMF_VARS.fd` (rather than pointing
directly at `/usr/share/OVMF/OVMF_VARS.fd`) because QEMU writes UEFI
variable state back into that file, and the system-wide copy should stay
read-only and shared across runs.

- [ ] **Step 6: Add a `cargo xtask` alias**

`.cargo/config.toml`:

```toml
[alias]
xtask = "run --package xtask --"
```

- [ ] **Step 7: Run it and verify the toolchain end-to-end**

```bash
wsl -d Ubuntu -- bash -lc "cd ~/projects/Rust_BL && source \$HOME/.cargo/env && cargo xtask run"
```

Expected: a QEMU window opens, briefly shows the UEFI shell/boot process
then the "Rust_BL bootloader: milestone 1 toolchain smoke test" log line,
the window closes on its own after ~2 seconds, and the command prints:

```
PASS: bootloader exited with expected code 33
```

with process exit code `0`. If QEMU opens no window (headless WSL without an
X server / WSLg), that's fine — the `PASS`/`FAIL` line and process exit code
are the actual pass/fail signal this task cares about; visually confirming
the log line on screen is optional and only possible if WSLg (or an X
server) is set up.

- [ ] **Step 8: Update the README to reflect the verified milestone**

In `README.md`, change:

```markdown
🚧 In progress — Milestone 1 of 6 complete (toolchain bootstrap).
```

to:

```markdown
✅ Milestone 1 of 6 complete and verified: `cargo xtask run` builds the
bootloader, boots it in QEMU/OVMF, and confirms a clean exit
(`PASS: bootloader exited with expected code 33`).
```

- [ ] **Step 9: Commit**

```bash
wsl -d Ubuntu -- bash -lc "cd ~/projects/Rust_BL && git add Cargo.toml bootloader xtask .cargo README.md && git commit -m 'Add xtask QEMU runner and verify Milestone 1 boot end-to-end'"
```

---

## After this plan

Milestone 1 is complete once Task 3 passes. The next plan (Milestone 2, per
the spec) covers: the bootloader's own ELF loader for a separate `kernel`
crate, obtaining the UEFI memory map and a Graphics Output Protocol
framebuffer, `exit_boot_services`, and handing off to a kernel entry stub —
at which point the `boot_info` shared crate is introduced. That plan should
be written fresh (via superpowers:writing-plans) once Milestone 1 is merged,
rather than pre-written here, since its exact shape depends on details only
visible after this toolchain is proven out.
