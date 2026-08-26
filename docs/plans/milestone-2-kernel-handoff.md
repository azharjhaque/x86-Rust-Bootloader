# Milestone 2: Bootloader → Kernel Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Milestone 1 smoke test into a real bootloader: read a
separate kernel ELF off the EFI System Partition, parse and load its
segments by hand, collect the UEFI memory map and a Graphics Output
Protocol framebuffer, exit UEFI boot services, and jump into the kernel on
a stack the bootloader allocated — with the kernel proving it is alive by
painting the framebuffer and reporting success itself.

**Architecture:** Four crates in the existing workspace. `bootloader`
(UEFI application) gains file I/O, a hand-written ELF64 loader, boot-info
collection, and the handoff jump. A new `kernel` crate builds to a
freestanding ELF for `x86_64-unknown-none`, linked at a fixed low address
by its own linker script, entered through a `sysv64` function that never
returns. A new `boot_info` crate holds the `#[repr(C)]` structs both sides
share, so the handoff ABI is one versioned contract rather than duplicated
offsets. `xtask` grows to build both binaries and stage both into the ESP.

**Tech Stack:** Rust nightly (pinned `nightly-2026-08-23`), `uefi` 0.39.0,
targets `x86_64-unknown-uefi` (bootloader) and `x86_64-unknown-none`
(kernel), QEMU + OVMF, inline `core::arch::asm!` only.

**Spec:** [docs/design.md](../design.md)

## Global Constraints

- Rust **nightly**, pinned to `nightly-2026-08-23` in `rust-toolchain.toml`.
- No `bootloader` crate from crates.io, and **no ELF-parsing crate** — the
  ELF64 header and program headers are parsed by hand. This is the point of
  the project.
- `xtask` stays dependency-free (`std` only). `kernel` and `boot_info` stay
  `#![no_std]` with no crates.io dependencies.
- QEMU + OVMF is the only tested target.
- All assembly is inline `core::arch::asm!` — no external assembler.
- Build/run orchestration lives in `xtask`, not shell scripts.
- Edition 2024 across all crates. Note that Rust 2024 requires
  `#[unsafe(no_mangle)]`, not bare `#[no_mangle]`.
- The exit-code contract from Milestone 1 is unchanged and load-bearing:
  `QemuExitCode::Success = 0x10` → QEMU process exit 33;
  `Failed = 0x11` → 35. Value 0 is deliberately unused (it maps to exit 1,
  indistinguishable from QEMU's own error exit).

## Deviation from the spec (decided during planning)

The spec calls for a **custom target JSON** for the kernel plus
`-Z build-std=core,alloc`. This plan uses the built-in
**`x86_64-unknown-none`** target instead: it is Tier 2, installable with
`rustup target add`, and already defaults to the `kernel` code model, no
red zone, and no SSE/AVX — precisely the properties the spec wanted the
JSON to express. Because it is Tier 2, `core` and `alloc` ship
precompiled, so `-Z build-std` is not needed either. This removes a
fragile file that tends to break across nightly releases. The spec's
intent is preserved; only the mechanism changed.

---

### Task 1: `boot_info` contract + `kernel` skeleton + `xtask` staging

**Files:**
- Create: `boot_info/Cargo.toml`
- Create: `boot_info/src/lib.rs`
- Create: `kernel/Cargo.toml`
- Create: `kernel/build.rs`
- Create: `kernel/kernel.ld`
- Create: `kernel/src/main.rs`
- Modify: `Cargo.toml` (workspace members)
- Modify: `rust-toolchain.toml` (add the kernel target)
- Modify: `xtask/src/main.rs` (build + stage the kernel)
- Modify: `README.md` (repository layout)

**Interfaces:**
- Produces: the `boot_info::BootInfo` type and its constants, consumed by
  every later task; `target/x86_64-unknown-none/debug/kernel` as an ELF
  staged into the ESP as `\kernel.elf`; a kernel entry symbol `_start`
  with signature `extern "sysv64" fn(*const BootInfo) -> !`.

- [ ] **Step 1: Create the `boot_info` crate manifest**

`boot_info/Cargo.toml`:

```toml
[package]
name = "boot_info"
version = "0.1.0"
edition = "2024"
publish = false
```

No dependencies — this crate is pure shared types.

- [ ] **Step 2: Define the handoff ABI**

`boot_info/src/lib.rs`:

```rust
//! The ABI contract between the bootloader and the kernel.
//!
//! Both sides depend on this crate so the handoff is one versioned type
//! rather than offsets duplicated in two places. Every type here is
//! `#[repr(C)]`: the kernel reads this struct from a raw pointer produced
//! by a separately compiled binary, so Rust's default layout rules must
//! not apply.

#![no_std]

/// Sanity value the kernel checks on entry. If this does not match, the
/// bootloader and kernel were built from mismatched versions of this
/// crate and the pointer must not be trusted.
pub const BOOT_INFO_MAGIC: u64 = 0x5255_5354_425f_4c30; // "RUSTB_L0"

/// Bumped whenever the layout of [`BootInfo`] changes incompatibly.
pub const BOOT_INFO_VERSION: u32 = 1;

/// How pixels are laid out in the framebuffer. Mirrors the two directly
/// drawable UEFI formats; bitmask and blt-only modes are rejected by the
/// bootloader rather than represented here.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormatKind {
    /// 32 bits per pixel, byte order R, G, B, reserved.
    Rgb = 0,
    /// 32 bits per pixel, byte order B, G, R, reserved.
    Bgr = 1,
}

/// Everything the kernel needs to draw to the screen.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrameBufferInfo {
    /// Physical address of the first pixel.
    pub addr: u64,
    /// Total size of the framebuffer in bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per scanline. May exceed `width` — always use this, not
    /// `width`, to step between rows.
    pub stride: u32,
    pub bytes_per_pixel: u32,
    pub pixel_format: PixelFormatKind,
    _pad: u32,
}

impl FrameBufferInfo {
    pub const fn new(
        addr: u64,
        size: u64,
        width: u32,
        height: u32,
        stride: u32,
        bytes_per_pixel: u32,
        pixel_format: PixelFormatKind,
    ) -> Self {
        Self {
            addr,
            size,
            width,
            height,
            stride,
            bytes_per_pixel,
            pixel_format,
            _pad: 0,
        }
    }
}

/// One entry of the UEFI memory map, flattened to a stable layout.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub start: u64,
    pub pages: u64,
    /// The raw UEFI `MemoryType` value. Milestone 5's frame allocator
    /// cares about `7` (`CONVENTIONAL`); everything else is reserved for
    /// now.
    pub kind: u32,
    _pad: u32,
}

impl MemoryRegion {
    pub const CONVENTIONAL: u32 = 7;

    pub const fn new(start: u64, pages: u64, kind: u32) -> Self {
        Self { start, pages, kind, _pad: 0 }
    }

    pub const fn is_usable(&self) -> bool {
        self.kind == Self::CONVENTIONAL
    }
}

/// The root handoff struct. The bootloader allocates this in memory that
/// survives `ExitBootServices` and passes a pointer to it in `rdi`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    _pad: u32,
    pub framebuffer: FrameBufferInfo,
    /// Pointer to an array of [`MemoryRegion`], valid for
    /// `memory_regions_len` entries.
    pub memory_regions_ptr: u64,
    pub memory_regions_len: u64,
    /// Lowest physical address of the loaded kernel image, and its span.
    pub kernel_base: u64,
    pub kernel_size: u64,
}

impl BootInfo {
    pub const fn new(
        framebuffer: FrameBufferInfo,
        memory_regions_ptr: u64,
        memory_regions_len: u64,
        kernel_base: u64,
        kernel_size: u64,
    ) -> Self {
        Self {
            magic: BOOT_INFO_MAGIC,
            version: BOOT_INFO_VERSION,
            _pad: 0,
            framebuffer,
            memory_regions_ptr,
            memory_regions_len,
            kernel_base,
            kernel_size,
        }
    }

    /// True if this struct came from a bootloader built against a
    /// compatible version of this crate.
    pub const fn is_valid(&self) -> bool {
        self.magic == BOOT_INFO_MAGIC && self.version == BOOT_INFO_VERSION
    }

    /// # Safety
    /// `memory_regions_ptr`/`len` must describe a live array, as they do
    /// when this struct came from the bootloader untouched.
    pub unsafe fn memory_regions(&self) -> &[MemoryRegion] {
        unsafe {
            core::slice::from_raw_parts(
                self.memory_regions_ptr as *const MemoryRegion,
                self.memory_regions_len as usize,
            )
        }
    }
}
```

- [ ] **Step 3: Create the `kernel` crate manifest**

`kernel/Cargo.toml`:

```toml
[package]
name = "kernel"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
boot_info = { path = "../boot_info" }

[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"
```

The `panic = "abort"` profiles matter: `x86_64-unknown-none` has no
unwinder, so an unwinding panic strategy fails to link.

- [ ] **Step 4: Write the kernel's linker script**

`kernel/kernel.ld`:

```
/* The kernel is loaded at a fixed low physical address that UEFI
   identity-maps, so no page-table work is needed in this milestone.
   2 MiB is above the legacy low-memory clutter and comfortably clear of
   anything OVMF places. */
ENTRY(_start)

SECTIONS {
    . = 2M;

    .text   : ALIGN(4K) { *(.text .text.*) }
    .rodata : ALIGN(4K) { *(.rodata .rodata.*) }
    .data   : ALIGN(4K) { *(.data .data.*) }
    .bss    : ALIGN(4K) { *(.bss .bss.*) *(COMMON) }

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) *(.comment) }
}
```

- [ ] **Step 5: Point the linker at that script from `build.rs`**

`kernel/build.rs`:

```rust
fn main() {
    // Use an absolute path derived from the manifest directory rather
    // than a relative one: the linker's working directory is not
    // guaranteed to be the workspace root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{manifest_dir}/kernel.ld");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-changed=build.rs");
}
```

- [ ] **Step 6: Write the kernel skeleton**

`kernel/src/main.rs`:

```rust
#![no_std]
#![no_main]

use boot_info::BootInfo;

/// The kernel's entry point.
///
/// The bootloader jumps here after `ExitBootServices` with a pointer to
/// [`BootInfo`] in `rdi` (the SysV first-argument register) and `rsp`
/// pointing at a stack the bootloader allocated for us.
///
/// # Safety
/// Called exactly once, by the bootloader, with a valid `BootInfo`
/// pointer. Never returns — there is nothing to return to.
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(_boot_info: *const BootInfo) -> ! {
    halt()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    halt()
}

/// Park the CPU. `hlt` in a loop rather than a bare spin so the core
/// idles instead of burning power; the loop guards against spurious
/// wakeups from interrupts.
fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) }
    }
}
```

- [ ] **Step 7: Add both new crates to the workspace**

`Cargo.toml` — update the `members` line, leaving `default-members` alone:

```toml
[workspace]
resolver = "2"
members = ["boot_info", "bootloader", "kernel", "xtask"]
default-members = ["xtask"]
```

- [ ] **Step 8: Add the kernel target to the toolchain file**

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "nightly-2026-08-23"
components = ["rust-src"]
targets = ["x86_64-unknown-uefi", "x86_64-unknown-none"]
```

- [ ] **Step 9: Build the kernel and confirm it is a valid ELF**

```bash
cargo build -p kernel --target x86_64-unknown-none
readelf -h target/x86_64-unknown-none/debug/kernel
```

Expected: `Type: EXEC`, `Machine: Advanced Micro Devices X86-64`, and an
`Entry point address` of `0x200000`. If the entry address is not
`0x200000`, the linker script was not applied — check `build.rs`.

- [ ] **Step 10: Confirm the entry symbol is exported unmangled**

```bash
readelf -s target/x86_64-unknown-none/debug/kernel | grep _start
```

Expected: a `_start` symbol. If it is absent or name-mangled, the
`#[unsafe(no_mangle)]` attribute is missing or misspelled.

- [ ] **Step 11: Teach `xtask` to build the kernel**

In `xtask/src/main.rs`, add a function beside `build_bootloader`, matching
its existing style and error handling:

```rust
fn build_kernel(root: &Path) -> Result<(), String> {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "kernel", "--target", "x86_64-unknown-none"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("failed to run cargo build for kernel: {e}"))?;
    if !status.success() {
        return Err("cargo build failed for kernel".to_string());
    }
    Ok(())
}
```

- [ ] **Step 12: Stage the kernel into the ESP alongside the bootloader**

In `stage_esp`, after the existing `bootloader.efi` copy, add the kernel
copy. The kernel goes at the ESP root as `kernel.elf`, not under
`EFI/BOOT`, so the bootloader's file-open path in Task 2 is a simple
top-level name:

```rust
    fs::copy(
        root.join("target/x86_64-unknown-none/debug/kernel"),
        root.join("target/esp/kernel.elf"),
    )
    .map_err(|e| format!("failed to copy kernel into the ESP (did the build produce it?): {e}"))?;
```

- [ ] **Step 13: Call `build_kernel` from `run`**

In `run()`, call `build_kernel(&root)` immediately after the existing
`build_bootloader(&root)` call, propagating its error the same way the
existing call does.

- [ ] **Step 14: Verify the full pipeline still passes**

```bash
cargo xtask run
```

Expected: still `PASS: bootloader exited with expected code 33`, exit 0.
The bootloader does not read the kernel yet — this step only proves the
new crates build and stage without disturbing Milestone 1's behavior.

- [ ] **Step 15: Confirm the kernel really landed in the ESP**

```bash
ls -la target/esp target/esp/EFI/BOOT
```

Expected: `kernel.elf` at the ESP root and `BOOTX64.EFI` under
`EFI/BOOT/`.

- [ ] **Step 16: Update the README's repository layout**

Add the two new crates to the layout block, and drop the "arrive in
Milestone 2" note since they have now arrived:

```
├── bootloader/   # UEFI application (PE32+), no_std, x86_64-unknown-uefi
├── boot_info/    # shared #[repr(C)] handoff ABI between the two
├── kernel/       # freestanding kernel ELF, no_std, x86_64-unknown-none
├── xtask/        # build automation: stages the ESP, launches QEMU
└── docs/         # design spec and per-milestone implementation plans
```

- [ ] **Step 17: Commit**

```bash
git add boot_info kernel Cargo.toml rust-toolchain.toml xtask/src/main.rs README.md
git commit -m "Add boot_info ABI crate and kernel skeleton, staged into the ESP"
```

---

### Task 2: Read `kernel.elf` off the EFI System Partition

**Files:**
- Modify: `bootloader/Cargo.toml` (enable the `alloc` feature)
- Create: `bootloader/src/file.rs`
- Modify: `bootloader/src/main.rs`

**Interfaces:**
- Consumes: `\kernel.elf` staged in the ESP by Task 1.
- Produces: `file::read_file(name: &CStr16) -> Result<Vec<u8>, Status>`,
  used by Task 3 to get the kernel image bytes.

- [ ] **Step 1: Enable `alloc` in the bootloader**

`bootloader/Cargo.toml` — the `uefi` crate's `alloc` feature enables `alloc`
crate types like `Vec`, but does not by itself install a global allocator;
that is the separate `global_allocator` feature, which backs it with UEFI
pool memory. The `logger` feature is also required for `log::info!`/
`log::error!` calls to reach the console at all — without it, `uefi::
helpers::init()` never registers a `log::Log` implementation and every log
call is silently a no-op:

```toml
[dependencies]
uefi = { version = "0.39.0", features = ["panic_handler", "alloc", "global_allocator", "logger"] }
log = "0.4"
```

- [ ] **Step 2: Write the file-reading module**

`bootloader/src/file.rs`:

```rust
//! Reading files from the EFI System Partition we were loaded from.

use alloc::vec;
use alloc::vec::Vec;

use uefi::boot;
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::{CStr16, Status};

/// Read an entire file from the root of the volume this image was loaded
/// from.
///
/// Returns [`Status::NOT_FOUND`] if the file does not exist,
/// [`Status::INVALID_PARAMETER`] if the name refers to a directory, and
/// [`Status::END_OF_FILE`] if the file is shorter than its own reported
/// size.
pub fn read_file(name: &CStr16) -> Result<Vec<u8>, Status> {
    // The image handle identifies the device we were booted from, so
    // this lands on the same ESP that holds BOOTX64.EFI.
    let mut fs = boot::get_image_file_system(boot::image_handle())
        .map_err(|e| e.status())?;
    let mut root = fs.open_volume().map_err(|e| e.status())?;

    let handle = root
        .open(name, FileMode::Read, FileAttribute::empty())
        .map_err(|e| e.status())?;

    // `open` succeeds for directories too; reading one as a regular file
    // is undefined, so reject it explicitly rather than trusting the name.
    let mut file: RegularFile = handle
        .into_regular_file()
        .ok_or(Status::INVALID_PARAMETER)?;

    // Ask the file how big it is rather than guessing a buffer size.
    let info = file.get_boxed_info::<FileInfo>().map_err(|e| e.status())?;
    let size = info.file_size() as usize;

    let mut buffer = vec![0u8; size];
    let mut filled = 0usize;
    while filled < size {
        let read = file.read(&mut buffer[filled..]).map_err(|e| e.status())?;
        if read == 0 {
            // EOF before we got the bytes the file's own metadata promised.
            // Returning a short buffer here would surface later as a
            // baffling ELF parse error, so fail where the cause is obvious.
            return Err(Status::END_OF_FILE);
        }
        filled += read;
    }

    Ok(buffer)
}
```

- [ ] **Step 3: Wire the module into `main.rs` and read the kernel**

In `bootloader/src/main.rs`, add the `alloc` extern crate declaration and
the new module, then read the kernel between the existing log line and
the exit call:

```rust
extern crate alloc;

mod file;
mod qemu_exit;
```

and in `main`:

```rust
    let kernel_name = cstr16!("kernel.elf");
    let kernel_image = match file::read_file(kernel_name) {
        Ok(image) => image,
        Err(status) => {
            log::error!("failed to read kernel.elf: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    log::info!("loaded kernel.elf: {} bytes", kernel_image.len());
```

`cstr16!` comes from `uefi::cstr16`; add it to the imports. Keep the
existing `boot::stall` line and the `Success` exit for now.

- [ ] **Step 4: Run and confirm the file was found and its size is sane**

```bash
cargo xtask run
```

Expected: the serial output contains a line like
`loaded kernel.elf: 17408 bytes` with a plausible non-zero size, followed
by `PASS: bootloader exited with expected code 33`.

If you see `failed to read kernel.elf: NOT_FOUND`, the staging step from
Task 1 did not run or wrote to the wrong path — check
`ls target/esp/kernel.elf`.

- [ ] **Step 5: Verify the reported size matches the real file**

```bash
stat -c %s target/esp/kernel.elf
```

Expected: exactly the byte count the bootloader logged. A mismatch means
the read was truncated and the `truncate` call is masking a short read.

- [ ] **Step 6: Commit**

```bash
git add bootloader
git commit -m "Read kernel.elf from the EFI System Partition"
```

---

### Task 3: Hand-written ELF64 loader

**Files:**
- Create: `bootloader/src/elf.rs`
- Modify: `bootloader/src/main.rs`

**Interfaces:**
- Consumes: the `Vec<u8>` kernel image from Task 2.
- Produces: `elf::load_kernel(image: &[u8]) -> Result<LoadedKernel, ElfError>`
  where `LoadedKernel { entry: u64, base: u64, size: u64 }`. Task 5 jumps
  to `entry` and reports `base`/`size` in `BootInfo`.

This is the heart of the milestone. No ELF crate — parse the bytes.

- [ ] **Step 1: Write the ELF loader**

`bootloader/src/elf.rs`:

```rust
//! A minimal ELF64 loader, written by hand.
//!
//! Only what a static, non-relocatable x86-64 kernel needs: validate the
//! header, walk the program headers, and copy every `PT_LOAD` segment to
//! the physical address it asked for. No relocations, no dynamic
//! linking, no section headers — a linked `ET_EXEC` image needs none of
//! them.

use core::ptr;

use uefi::boot::{self, AllocateType};
use uefi::mem::memory_map::MemoryType;

/// 4 KiB, the only page size this loader uses.
const PAGE_SIZE: u64 = 4096;

// ELF identification indices and values (see the ELF64 spec).
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3e;
const PT_LOAD: u32 = 1;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    NotExecutable,
    WrongArchitecture,
    BadProgramHeaders,
    /// A segment claimed more bytes in the file than the file contains.
    SegmentOutOfBounds,
    /// `p_filesz` exceeded `p_memsz`, which is malformed.
    SegmentTooLarge,
    /// No `PT_LOAD` segments at all — nothing to load.
    NoLoadableSegments,
    /// UEFI refused to give us the pages the segment asked for.
    AllocationFailed,
}

/// Where a loaded kernel ended up in physical memory.
#[derive(Debug, Clone, Copy)]
pub struct LoadedKernel {
    /// Virtual (here: physical, since we are identity-mapped) entry point.
    pub entry: u64,
    /// Lowest address of any loaded segment, rounded down to a page.
    pub base: u64,
    /// Span from `base` to the end of the highest segment.
    pub size: u64,
}

/// Read a little-endian `u16`/`u32`/`u64` at `offset`, or `None` if the
/// slice is too short. Hand-rolled rather than using `from_le_bytes` on a
/// fixed array so a truncated file returns an error instead of panicking.
/// The end of the read is computed with `checked_add` before indexing so a
/// near-`usize::MAX` offset cannot wrap around and pass the bounds check.
fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = bytes.get(offset..end)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let slice = bytes.get(offset..end)?;
    Some(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

/// One `PT_LOAD` program header, after parsing.
#[derive(Debug, Clone, Copy)]
struct LoadSegment {
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

/// Validate the ELF header and return `(entry, phoff, phentsize, phnum)`.
fn parse_header(image: &[u8]) -> Result<(u64, u64, u16, u16), ElfError> {
    if image.len() < EHDR_SIZE {
        return Err(ElfError::TooSmall);
    }
    if image[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    if image[EI_CLASS] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }
    if image[EI_DATA] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }

    let e_type = read_u16(image, 16).ok_or(ElfError::TooSmall)?;
    if e_type != ET_EXEC {
        // A PIE kernel would need relocation processing we deliberately
        // do not implement; the linker script produces ET_EXEC.
        return Err(ElfError::NotExecutable);
    }

    let e_machine = read_u16(image, 18).ok_or(ElfError::TooSmall)?;
    if e_machine != EM_X86_64 {
        return Err(ElfError::WrongArchitecture);
    }

    let e_entry = read_u64(image, 24).ok_or(ElfError::TooSmall)?;
    let e_phoff = read_u64(image, 32).ok_or(ElfError::TooSmall)?;
    let e_phentsize = read_u16(image, 54).ok_or(ElfError::TooSmall)?;
    let e_phnum = read_u16(image, 56).ok_or(ElfError::TooSmall)?;

    if e_phentsize as usize != PHDR_SIZE {
        return Err(ElfError::BadProgramHeaders);
    }

    Ok((e_entry, e_phoff, e_phentsize, e_phnum))
}

/// Load every `PT_LOAD` segment of `image` into the physical memory it
/// requests, zeroing the `.bss` tail, and report where it landed.
pub fn load_kernel(image: &[u8]) -> Result<LoadedKernel, ElfError> {
    let (entry, phoff, phentsize, phnum) = parse_header(image)?;

    let mut lowest = u64::MAX;
    let mut highest = 0u64;
    let mut loaded_any = false;

    for index in 0..phnum as usize {
        // Compute this header's offset with checked arithmetic in u64: a
        // crafted `e_phoff`/`e_phnum` must not be able to wrap the
        // computation and land `base` somewhere bogus but in-bounds.
        let base = phoff
            .checked_add(
                (index as u64)
                    .checked_mul(phentsize as u64)
                    .ok_or(ElfError::BadProgramHeaders)?,
            )
            .ok_or(ElfError::BadProgramHeaders)? as usize;

        // Check the whole 56-byte header is present before reading any of
        // its fields.
        let header_end = base.checked_add(PHDR_SIZE).ok_or(ElfError::BadProgramHeaders)?;
        if header_end > image.len() {
            return Err(ElfError::BadProgramHeaders);
        }

        let p_type = read_u32(image, base).ok_or(ElfError::BadProgramHeaders)?;
        if p_type != PT_LOAD {
            continue;
        }

        let segment = LoadSegment {
            offset: read_u64(image, base + 8).ok_or(ElfError::BadProgramHeaders)?,
            vaddr: read_u64(image, base + 16).ok_or(ElfError::BadProgramHeaders)?,
            filesz: read_u64(image, base + 32).ok_or(ElfError::BadProgramHeaders)?,
            memsz: read_u64(image, base + 40).ok_or(ElfError::BadProgramHeaders)?,
        };

        if segment.filesz > segment.memsz {
            return Err(ElfError::SegmentTooLarge);
        }

        let file_end = segment
            .offset
            .checked_add(segment.filesz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if file_end > image.len() as u64 {
            return Err(ElfError::SegmentOutOfBounds);
        }

        // Compute the segment's memory end BEFORE loading: this same
        // overflow would otherwise wrap `padding + memsz` inside
        // load_segment and under-allocate the destination.
        let seg_start = align_down(segment.vaddr);
        let seg_end = segment
            .vaddr
            .checked_add(segment.memsz)
            .ok_or(ElfError::SegmentOutOfBounds)?;

        load_segment(image, &segment)?;
        loaded_any = true;

        lowest = lowest.min(seg_start);
        highest = highest.max(seg_end);
    }

    if !loaded_any {
        return Err(ElfError::NoLoadableSegments);
    }

    Ok(LoadedKernel {
        entry,
        base: lowest,
        size: highest - lowest,
    })
}

fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

/// Allocate the pages one segment needs and copy it into place.
fn load_segment(image: &[u8], segment: &LoadSegment) -> Result<(), ElfError> {
    // `p_vaddr` is not required to be page-aligned, but UEFI allocates
    // whole pages at page-aligned addresses. Round down and remember how
    // far into the first page the segment actually starts.
    let page_start = align_down(segment.vaddr);
    let padding = segment.vaddr - page_start;
    // This loader is called after `load_kernel` has already validated
    // `vaddr.checked_add(memsz)`, but that guard belongs to the caller —
    // guard here too so this function stays safe on its own, independent
    // of call order.
    let total = padding
        .checked_add(segment.memsz)
        .ok_or(ElfError::SegmentOutOfBounds)?;

    // A legal but empty `PT_LOAD` segment (`p_memsz == 0`) needs no
    // allocation at all. `allocate_pages` rejects a zero page count, so
    // without this early return a harmless empty segment would abort the
    // whole load with a misleading `AllocationFailed`.
    if total == 0 {
        return Ok(());
    }

    let pages = total.div_ceil(PAGE_SIZE) as usize;

    // `AllocateType::Address` demands these exact pages. If UEFI has
    // already put something there, this fails rather than silently
    // loading the kernel somewhere it was not linked for.
    boot::allocate_pages(
        AllocateType::Address(page_start),
        MemoryType::LOADER_DATA,
        pages,
    )
    .map_err(|_| ElfError::AllocationFailed)?;

    // SAFETY: `allocate_pages(AllocateType::Address(page_start), ...)`
    // above succeeded, which means UEFI just handed us `pages` fresh,
    // free pages starting at `page_start` — so the whole
    // `[page_start, page_start + pages * PAGE_SIZE)` range is valid to
    // write through a raw pointer. `image` is a live `Vec<u8>` allocation
    // made by the boot-time global allocator, entirely separate from the
    // page-allocator's memory map, so `src`'s `filesz`-byte range cannot
    // overlap `dst`'s range inside the pages we were just granted,
    // satisfying `copy_nonoverlapping`'s non-overlap requirement. The
    // pages are ordinary conventional (identity-mapped, no active paging
    // yet) memory, so they are both readable and writable.
    unsafe {
        // Zero the whole allocation first. That covers the padding at the
        // front, the .bss tail at the back, and any gap in between — one
        // operation instead of three fiddly ranges.
        ptr::write_bytes(page_start as *mut u8, 0, (pages as u64 * PAGE_SIZE) as usize);

        let src = image.as_ptr().add(segment.offset as usize);
        let dst = segment.vaddr as *mut u8;
        ptr::copy_nonoverlapping(src, dst, segment.filesz as usize);
    }

    Ok(())
}
```

- [ ] **Step 2: Call the loader from `main.rs`**

Add `mod elf;` alongside the other modules, and after the successful
read:

```rust
    let loaded = match elf::load_kernel(&kernel_image) {
        Ok(loaded) => loaded,
        Err(error) => {
            log::error!("failed to load kernel ELF: {error:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    log::info!(
        "kernel loaded: entry={:#x} base={:#x} size={:#x}",
        loaded.entry,
        loaded.base,
        loaded.size
    );
```

- [ ] **Step 3: Run and confirm the loader agrees with the linker**

```bash
cargo xtask run
```

Expected: a line like
`kernel loaded: entry=0x200000 base=0x200000 size=0x...` followed by
`PASS`. The entry address must be `0x200000` — the same value
`readelf -h` reported in Task 1.

- [ ] **Step 4: Cross-check against `readelf`**

```bash
readelf -l target/x86_64-unknown-none/debug/kernel
```

Expected: the `LOAD` segments' `VirtAddr` range matches the `base` and
`size` the bootloader logged. This is the check that proves the
hand-written parser reads the same structure the toolchain wrote.

- [ ] **Step 5: Prove the loader rejects malformed input**

Temporarily corrupt the magic to confirm the validation path fires, then
restore it:

```bash
cp target/esp/kernel.elf /tmp/kernel.elf.bak
printf 'XXXX' | dd of=target/esp/kernel.elf bs=1 seek=0 conv=notrunc
```

Then run QEMU directly against the already-staged ESP, so `xtask` does not
rebuild and overwrite the corrupted file:

```bash
qemu-system-x86_64 \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=target/OVMF_VARS.fd \
  -drive format=raw,file=fat:rw:target/esp \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  -m 256M -display none -serial stdio
echo "qemu exit: $?"
```

Expected: `failed to load kernel ELF: BadMagic` in the serial output and
`qemu exit: 35` — the `Failed` path, proving the error branch works end
to end. Then restore:

```bash
cp /tmp/kernel.elf.bak target/esp/kernel.elf
```

- [ ] **Step 6: Commit**

```bash
git add bootloader
git commit -m "Add hand-written ELF64 loader for the kernel image"
```

---

### Task 4: Collect framebuffer and memory map into `BootInfo`

**Files:**
- Modify: `bootloader/Cargo.toml` (depend on `boot_info`)
- Create: `bootloader/src/graphics.rs`
- Create: `bootloader/src/memory.rs`
- Modify: `bootloader/src/main.rs`

**Interfaces:**
- Consumes: `LoadedKernel` from Task 3, `boot_info` types from Task 1.
- Produces: `graphics::open_framebuffer() -> Result<FrameBufferInfo, Status>`
  and `memory::allocate_region_array(capacity) -> Result<(*mut MemoryRegion, usize), Status>`,
  both consumed by Task 5.

- [ ] **Step 1: Depend on the shared ABI crate**

`bootloader/Cargo.toml`:

```toml
[dependencies]
uefi = { version = "0.39.0", features = ["panic_handler", "alloc"] }
boot_info = { path = "../boot_info" }
log = "0.4"
```

- [ ] **Step 2: Write the framebuffer module**

`bootloader/src/graphics.rs`:

```rust
//! Obtaining a linear framebuffer via the Graphics Output Protocol.

use boot_info::{FrameBufferInfo, PixelFormatKind};
use uefi::boot::{self, ScopedProtocol};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::Status;

/// Open the GOP and describe the current mode's framebuffer.
///
/// The returned addresses stay valid after `ExitBootServices` because the
/// framebuffer is memory-mapped hardware, not boot-services memory.
pub fn open_framebuffer() -> Result<FrameBufferInfo, Status> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>()
        .map_err(|e| e.status())?;
    let mut gop: ScopedProtocol<GraphicsOutput> =
        boot::open_protocol_exclusive::<GraphicsOutput>(handle).map_err(|e| e.status())?;

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();

    // Only the two directly drawable formats are supported. Bitmask and
    // blt-only modes would need a conversion path the kernel does not
    // have, so refuse rather than render garbage.
    let pixel_format = match mode.pixel_format() {
        PixelFormat::Rgb => PixelFormatKind::Rgb,
        PixelFormat::Bgr => PixelFormatKind::Bgr,
        PixelFormat::Bitmask | PixelFormat::BltOnly => return Err(Status::UNSUPPORTED),
    };

    let mut framebuffer = gop.frame_buffer();
    let addr = framebuffer.as_mut_ptr() as u64;
    let size = framebuffer.size() as u64;

    Ok(FrameBufferInfo::new(
        addr,
        size,
        width as u32,
        height as u32,
        stride as u32,
        4, // both supported formats are 32 bits per pixel
        pixel_format,
    ))
}
```

- [ ] **Step 3: Write the memory-region module**

`bootloader/src/memory.rs`:

```rust
//! Allocating the storage the memory map will be copied into.
//!
//! The ordering here is subtle and worth stating plainly: the final
//! memory map only exists *after* `ExitBootServices`, but allocation is
//! only possible *before* it. So we allocate a generously sized array up
//! front and fill it in afterwards.

use boot_info::MemoryRegion;
use uefi::boot::{self, AllocateType};
use uefi::mem::memory_map::MemoryType;
use uefi::Status;

const PAGE_SIZE: usize = 4096;

/// Allocate space for `capacity` [`MemoryRegion`] entries in memory that
/// survives `ExitBootServices`.
///
/// Returns the array pointer and the capacity it can actually hold, which
/// is rounded up to a whole number of pages.
pub fn allocate_region_array(capacity: usize) -> Result<(*mut MemoryRegion, usize), Status> {
    let entry_size = size_of::<MemoryRegion>();
    let bytes = capacity * entry_size;
    let pages = bytes.div_ceil(PAGE_SIZE);

    // LOADER_DATA is preserved across ExitBootServices; BOOT_SERVICES_DATA
    // would be reclaimed out from under the kernel.
    let ptr = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .map_err(|e| e.status())?;

    let actual_capacity = (pages * PAGE_SIZE) / entry_size;
    Ok((ptr.as_ptr().cast::<MemoryRegion>(), actual_capacity))
}

/// How many entries to reserve room for.
///
/// A QEMU boot reports well under 100 regions; `ExitBootServices` itself
/// can split a region or two, so 256 leaves generous headroom while
/// costing only a few pages.
pub const REGION_CAPACITY: usize = 256;
```

- [ ] **Step 4: Call both from `main.rs`**

Add `mod graphics;` and `mod memory;`, then after the ELF load:

```rust
    let framebuffer = match graphics::open_framebuffer() {
        Ok(info) => info,
        Err(status) => {
            log::error!("failed to open framebuffer: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    log::info!(
        "framebuffer: {}x{} stride={} addr={:#x} size={:#x} format={:?}",
        framebuffer.width,
        framebuffer.height,
        framebuffer.stride,
        framebuffer.addr,
        framebuffer.size,
        framebuffer.pixel_format
    );

    let (regions_ptr, regions_capacity) =
        match memory::allocate_region_array(memory::REGION_CAPACITY) {
            Ok(pair) => pair,
            Err(status) => {
                log::error!("failed to allocate memory-region array: {status:?}");
                qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
            }
        };

    log::info!("memory-region array: capacity={regions_capacity}");
```

- [ ] **Step 5: Run and confirm both succeed**

```bash
cargo xtask run
```

Expected: a `framebuffer:` line with a plausible resolution (OVMF
typically gives `800x600` or `1024x768`), a non-zero address, format
`Bgr` under OVMF, and a `memory-region array: capacity=...` line of at
least 256 — then `PASS`.

If the framebuffer step fails with `UNSUPPORTED`, the firmware offered
only a blt-only mode; report it rather than working around it, since the
kernel's drawing code depends on a linear framebuffer.

- [ ] **Step 6: Commit**

```bash
git add bootloader
git commit -m "Collect GOP framebuffer and reserve memory-map storage"
```

---

### Task 5: Exit boot services and jump to the kernel

**Files:**
- Create: `bootloader/src/handoff.rs`
- Modify: `bootloader/src/main.rs`
- Modify: `kernel/src/main.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything the previous tasks produced.
- Produces: a booted kernel. After this task the `PASS` signal originates
  in the **kernel**, not the bootloader — that is the milestone's proof.

- [ ] **Step 1: Write the handoff module**

`bootloader/src/handoff.rs`:

```rust
//! The point of no return: leave UEFI behind and enter the kernel.

use core::arch::asm;

use boot_info::{BootInfo, MemoryRegion};
use uefi::boot::{self, AllocateType};
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::Status;

const PAGE_SIZE: usize = 4096;

/// 64 KiB of stack for the kernel. The UEFI stack we are currently
/// running on lives in boot-services memory, which stops being ours the
/// moment `ExitBootServices` returns — so the kernel needs its own.
const KERNEL_STACK_PAGES: usize = 16;

/// Allocate a stack for the kernel and return the address to load into
/// `rsp` — the *top*, since x86 stacks grow downward.
pub fn allocate_kernel_stack() -> Result<u64, Status> {
    let ptr = boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        KERNEL_STACK_PAGES,
    )
    .map_err(|e| e.status())?;

    let bottom = ptr.as_ptr() as u64;
    let top = bottom + (KERNEL_STACK_PAGES * PAGE_SIZE) as u64;

    // The SysV ABI wants 16-byte alignment at the call boundary.
    Ok(top & !0xf)
}

/// Exit boot services, fill in the memory map, and jump to the kernel.
///
/// # Safety
/// `boot_info_ptr` must point at storage that survives
/// `ExitBootServices`, `regions_ptr` must have room for
/// `regions_capacity` entries, `entry` must be the kernel's entry point,
/// and `stack_top` must be a valid stack. Nothing may hold a
/// boot-services reference at this point.
pub unsafe fn exit_and_jump(
    boot_info_ptr: *mut BootInfo,
    regions_ptr: *mut MemoryRegion,
    regions_capacity: usize,
    entry: u64,
    stack_top: u64,
) -> ! {
    // After this call, every UEFI boot service is gone — including the
    // logger. Nothing below may log.
    //
    // The returned map is allocated in LOADER_DATA by the uefi crate
    // precisely so it outlives this call.
    let memory_map = unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };

    let mut count = 0usize;
    // Note the name: `descriptor`, not `entry` — `entry` is this
    // function's kernel-entry-point parameter, used in the asm below.
    for descriptor in memory_map.entries() {
        if count >= regions_capacity {
            // Out of room. Truncating is the only option left: we cannot
            // allocate now, and we cannot report an error to anyone.
            break;
        }
        unsafe {
            regions_ptr.add(count).write(MemoryRegion::new(
                descriptor.phys_start,
                descriptor.page_count,
                descriptor.ty.0,
            ));
        }
        count += 1;
    }

    unsafe {
        (*boot_info_ptr).memory_regions_len = count as u64;
    }

    unsafe {
        asm!(
            "mov rsp, {stack}",
            // Clear the frame pointer so a backtrace walker stops here
            // rather than wandering into UEFI's dead stack frames.
            "xor rbp, rbp",
            "jmp {entry}",
            stack = in(reg) stack_top,
            entry = in(reg) entry,
            // SysV puts the first argument in rdi.
            in("rdi") boot_info_ptr,
            options(noreturn)
        )
    }
}
```

- [ ] **Step 2: Allocate and populate `BootInfo` in `main.rs`**

Add `mod handoff;` and, after the memory-region array is allocated:

```rust
    let stack_top = match handoff::allocate_kernel_stack() {
        Ok(top) => top,
        Err(status) => {
            log::error!("failed to allocate kernel stack: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    // BootInfo itself must outlive boot services, so it goes in
    // LOADER_DATA rather than on our soon-to-be-invalid stack.
    let boot_info_ptr = match boot::allocate_pages(
        uefi::boot::AllocateType::AnyPages,
        uefi::mem::memory_map::MemoryType::LOADER_DATA,
        1,
    ) {
        Ok(ptr) => ptr.as_ptr().cast::<BootInfo>(),
        Err(status) => {
            log::error!("failed to allocate BootInfo: {status:?}");
            qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
        }
    };

    unsafe {
        boot_info_ptr.write(BootInfo::new(
            framebuffer,
            regions_ptr as u64,
            0, // filled in after ExitBootServices, when the map is final
            loaded.base,
            loaded.size,
        ));
    }

    log::info!("handing off to kernel at {:#x}", loaded.entry);

    unsafe {
        handoff::exit_and_jump(
            boot_info_ptr,
            regions_ptr,
            regions_capacity,
            loaded.entry,
            stack_top,
        )
    }
```

Import `boot_info::BootInfo` at the top. Delete the now-unreachable
`boot::stall` line and the trailing `qemu_exit::exit(Success)` — the
bootloader no longer exits on its own, and leaving them would be dead
code.

- [ ] **Step 3: Make the kernel prove it is alive**

Replace `kernel/src/main.rs`'s `_start` body. The kernel validates the
handoff, paints the screen, and reports success itself:

```rust
#![no_std]
#![no_main]

use boot_info::{BootInfo, PixelFormatKind};

mod qemu_exit;

#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    // The pointer comes from another binary, so check the contract before
    // trusting anything behind it.
    if boot_info.is_null() {
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    let info = unsafe { &*boot_info };
    if !info.is_valid() {
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }

    fill_screen(info, 0x00, 0x33, 0x99);

    qemu_exit::exit(qemu_exit::QemuExitCode::Success)
}

/// Paint the whole framebuffer one colour — the simplest possible proof
/// that we reached the kernel and the framebuffer description is right.
fn fill_screen(info: &BootInfo, red: u8, green: u8, blue: u8) {
    let fb = &info.framebuffer;
    let pixel = match fb.pixel_format {
        PixelFormatKind::Rgb => [red, green, blue, 0],
        PixelFormatKind::Bgr => [blue, green, red, 0],
    };

    let base = fb.addr as *mut u8;
    for y in 0..fb.height as usize {
        for x in 0..fb.width as usize {
            let offset = (y * fb.stride as usize + x) * fb.bytes_per_pixel as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(pixel.as_ptr(), base.add(offset), 4);
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    qemu_exit::exit(qemu_exit::QemuExitCode::Failed)
}
```

- [ ] **Step 4: Give the kernel its own copy of the exit helper**

Create `kernel/src/qemu_exit.rs` with the same content as
`bootloader/src/qemu_exit.rs`. Copy it verbatim, including the comments.

This is deliberate duplication of about twenty lines, and it is worth
naming why: the two binaries are compiled for different targets and share
only `boot_info`, whose job is the handoff ABI. Moving port I/O into that
crate would widen its purpose from "what the bootloader tells the kernel"
to "shared utilities," which is the kind of drift that turns a clean
contract into a junk drawer. If a third consumer ever appears, extract a
`qemu_exit` crate then.

- [ ] **Step 5: Run it — the milestone's payoff**

```bash
cargo xtask run
```

Expected serial output ending with the handoff line, then
`PASS: bootloader exited with expected code 33` and exit 0.

The crucial difference from every previous run: that `PASS` was written
by the **kernel**, after `ExitBootServices`, on a stack the bootloader
allocated. The bootloader never exits on its own any more.

- [ ] **Step 6: See it with your own eyes**

The framebuffer fill is invisible with `-display none`. Confirm it once
by running QEMU directly with a window (requires WSLg or an X server):

```bash
qemu-system-x86_64 \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=target/OVMF_VARS.fd \
  -drive format=raw,file=fat:rw:target/esp \
  -m 256M
```

Expected: the screen turns solid blue and stays there (without
`isa-debug-exit` the kernel's port write does nothing, so it simply
halts). Close the window when done.

If no display is available, skip this step — the scripted `PASS` is the
authoritative signal. Do not block on it.

- [ ] **Step 7: Confirm the failure path still works**

Corrupt the magic again as in Task 3 Step 5 and run QEMU directly against
the staged ESP. Expected: exit code 35, not 33 — the bootloader's
`Failed` path.

- [ ] **Step 8: Update the README**

Change the Status block to record Milestone 2, and tick its roadmap entry:

```markdown
✅ Milestone 2 of 6 complete and verified: the bootloader loads a
separate kernel ELF from the EFI System Partition with a hand-written
ELF64 loader, collects the UEFI memory map and a GOP framebuffer, exits
boot services, and jumps to the kernel — which paints the framebuffer and
reports success itself.
```

and:

```markdown
- [x] 2. Bootloader: ELF loader, memory map, framebuffer, handoff to kernel
```

Only after Step 5 has actually passed.

- [ ] **Step 9: Commit**

```bash
git add bootloader kernel README.md
git commit -m "Exit boot services and hand off to the kernel"
```

---

## After this plan

Milestone 2 is complete when Task 5 passes. Milestone 3 (GDT, IDT,
double-fault handler) builds directly on the kernel entry point this plan
establishes. Two things this plan deliberately left for later, so they are
not surprises:

- **The kernel runs on UEFI's page tables.** It is identity-mapped and
  linked at 2 MiB, which is why no paging work was needed here. Building
  the kernel's own page tables is a Milestone 3+ concern.
- **Serial output stops at the handoff.** Once boot services are gone, the
  `log` crate has no backend, so the kernel is silent apart from the exit
  code and the framebuffer. A serial-port driver written directly against
  COM1 is the natural first task of Milestone 3 — it makes everything
  after it far easier to debug.
