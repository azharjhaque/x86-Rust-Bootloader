# Milestone 6: Frame Allocator + Kernel Heap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the kernel working `alloc` types (`Box`, `Vec`, `String`) backed by a hand-rolled physical frame allocator and a hand-rolled coalescing heap.

**Architecture:** Three layers. `boot_info::UsableRegions` turns the UEFI memory map into page-aligned frame runs (pure, host-tested). `kernel::frame` bump-allocates 4 KiB frames from those runs with an intrusive free stack for reclaimed frames. `kernel::heap` takes 256 contiguous frames from `frame` and runs an address-ordered free list with splitting and bidirectional coalescing behind `#[global_allocator]`.

**Tech Stack:** Rust `nightly-2026-08-23`, edition 2024, `no_std`, target `x86_64-unknown-none`. No new dependencies — everything hand-rolled.

**Spec:** `docs/superpowers/specs/2026-08-27-frame-heap-allocator-design.md`

## Global Constraints

These apply to every task. They are not repeated per task.

- **Build and run everything in WSL** at `/home/coolg/projects/Rust_BL`. QEMU and OVMF only exist there. The Windows checkout is the push path, not a build environment.
- **Branch:** `milestone-6-frame-allocator`, already created, already carrying the spec commits.
- **No new crate dependencies.** Both allocators are hand-rolled; that is the point of the milestone.
- **Edition 2024 forbids references to `static mut`.** Use `&raw mut NAME` / `&raw const NAME`, matching `kernel/src/console.rs:55` and `kernel/src/idt.rs:136`.
- **Every failure path reports on serial, then exits.** `kprintln!` a line naming the failure, then `qemu_exit::exit(QemuExitCode::Failed)`. Never fail silently, never `panic!` where a message plus a clean exit will do.
- **Allocator init goes after `console::init` and before `interrupts::enable()`.** Both halves are load-bearing — see Task 3, Step 1.
- **Comment style:** this codebase explains *why*, not *what*, and records rejected alternatives. Match it. Every `unsafe` block carries a `// SAFETY:` comment naming the invariant that makes it sound.
- **Regression bar, unchanged by this milestone:** `cargo test -p xtask` (3 pass), `cargo xtask run` (exit 33 + glyph), `cargo xtask test` (exit 35). No `xtask` source changes are required or expected.

---

### Task 1: `UsableRegions` — page-aligned frame runs from the memory map

The one piece of this milestone that is pure arithmetic, so it is the one piece that gets real red-green host tests. It lives in `boot_info` because it operates entirely on `MemoryRegion`, which is defined there, and because `cargo test -p boot_info` already builds a host test harness (verified — the crate is `#![no_std]` and tests fine anyway).

**Files:**
- Modify: `boot_info/src/lib.rs` (append new items; add `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `MemoryRegion`, `MemoryRegion::is_usable()` — both already exist.
- Produces:
  - `pub const PAGE_SIZE: u64 = 4096;`
  - `pub struct FrameRun { pub start: u64, pub frames: u64 }` — `Clone, Copy, Debug, PartialEq, Eq`
  - `pub struct UsableRegions<'a>` — `Clone, Copy, Debug`
  - `pub const fn UsableRegions::new(regions: &'a [MemoryRegion]) -> Self`
  - `impl Iterator for UsableRegions<'a> { type Item = FrameRun; }`

- [ ] **Step 1: Write the failing tests**

Append to `boot_info/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A UEFI type that is not CONVENTIONAL. 2 is LOADER_DATA, which is
    /// exactly what the bootloader marks the kernel image, stack, and
    /// BootInfo as — so this is the case that keeps the allocator from
    /// handing out memory the kernel is still running on.
    const LOADER_DATA: u32 = 2;

    #[test]
    fn skips_regions_that_are_not_conventional() {
        let regions = [
            MemoryRegion::new(0x1000, 4, LOADER_DATA),
            MemoryRegion::new(0x5000, 4, MemoryRegion::CONVENTIONAL),
        ];
        let mut runs = UsableRegions::new(&regions);
        assert_eq!(runs.next(), Some(FrameRun { start: 0x5000, frames: 4 }));
        assert_eq!(runs.next(), None);
    }

    #[test]
    fn never_yields_the_frame_at_physical_zero() {
        // A run starting at 0 must be trimmed to start at 0x1000, so an
        // allocated frame address is never null.
        let regions = [MemoryRegion::new(0, 2, MemoryRegion::CONVENTIONAL)];
        let mut runs = UsableRegions::new(&regions);
        assert_eq!(runs.next(), Some(FrameRun { start: 0x1000, frames: 1 }));
        assert_eq!(runs.next(), None);
    }

    #[test]
    fn clamps_an_unaligned_span_inward() {
        // 0x1800..0x3800 contains exactly one whole aligned frame.
        let regions = [MemoryRegion::new(0x1800, 2, MemoryRegion::CONVENTIONAL)];
        let mut runs = UsableRegions::new(&regions);
        assert_eq!(runs.next(), Some(FrameRun { start: 0x2000, frames: 1 }));
        assert_eq!(runs.next(), None);
    }

    #[test]
    fn drops_a_span_that_rounds_away_to_nothing() {
        // 0x1800..0x2800 straddles a boundary but contains no whole frame.
        let regions = [MemoryRegion::new(0x1800, 1, MemoryRegion::CONVENTIONAL)];
        assert_eq!(UsableRegions::new(&regions).next(), None);
    }

    #[test]
    fn yields_every_usable_region_in_order() {
        let regions = [
            MemoryRegion::new(0x2000, 3, MemoryRegion::CONVENTIONAL),
            MemoryRegion::new(0x9000, 1, LOADER_DATA),
            MemoryRegion::new(0xa000, 2, MemoryRegion::CONVENTIONAL),
        ];
        let mut runs = UsableRegions::new(&regions);
        assert_eq!(runs.next(), Some(FrameRun { start: 0x2000, frames: 3 }));
        assert_eq!(runs.next(), Some(FrameRun { start: 0xa000, frames: 2 }));
        assert_eq!(runs.next(), None);
    }

    #[test]
    fn an_empty_map_yields_nothing() {
        assert_eq!(UsableRegions::new(&[]).next(), None);
    }

    #[test]
    fn skips_a_descriptor_whose_span_overflows() {
        // A malformed descriptor must be skipped, not trusted and not
        // panicked on: this data crosses a binary boundary.
        let regions = [MemoryRegion::new(u64::MAX - 0xfff, u64::MAX, MemoryRegion::CONVENTIONAL)];
        assert_eq!(UsableRegions::new(&regions).next(), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo test -p boot_info'
```

Expected: compile error — `cannot find type UsableRegions in this scope` (and `FrameRun`, `PAGE_SIZE`). That is the correct failure; the types do not exist yet.

- [ ] **Step 3: Write the implementation**

Append to `boot_info/src/lib.rs`, above the `#[cfg(test)]` module:

```rust
/// The only page size this project deals in. UEFI reports region sizes in
/// units of this, and the kernel's frame allocator hands out exactly this.
pub const PAGE_SIZE: u64 = 4096;

/// A contiguous, page-aligned span of physical memory that is free for the
/// kernel to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRun {
    pub start: u64,
    pub frames: u64,
}

/// Iterator over the usable, page-aligned runs of a UEFI memory map.
///
/// This is deliberately pure — no raw pointers, no statics, no `unsafe` —
/// so that the fiddly half of the frame allocator (which region survives,
/// where does it start, how many whole frames does it really contain) is
/// testable on the host, where a failure is a red test rather than a
/// triple fault with no logger left to report it.
#[derive(Clone, Copy, Debug)]
pub struct UsableRegions<'a> {
    regions: &'a [MemoryRegion],
    index: usize,
}

impl<'a> UsableRegions<'a> {
    pub const fn new(regions: &'a [MemoryRegion]) -> Self {
        Self { regions, index: 0 }
    }
}

impl Iterator for UsableRegions<'_> {
    type Item = FrameRun;

    fn next(&mut self) -> Option<FrameRun> {
        while self.index < self.regions.len() {
            let region = self.regions[self.index];
            self.index += 1;

            if !region.is_usable() {
                continue;
            }

            // This data crossed a binary boundary and is not trusted.
            // A descriptor whose span overflows is skipped rather than
            // wrapped around into a bogus low run.
            let Some(span) = region.pages.checked_mul(PAGE_SIZE) else {
                continue;
            };
            let Some(end) = region.start.checked_add(span) else {
                continue;
            };

            // Never yield the frame at physical 0. Downstream the frame
            // address becomes a `NonNull`, and a null frame would make
            // "no frame" and "the first frame" indistinguishable.
            let Some(start) = region.start.max(PAGE_SIZE).checked_next_multiple_of(PAGE_SIZE)
            else {
                continue;
            };
            let end = end - (end % PAGE_SIZE);

            // Rounding inward can consume the whole region.
            if end <= start {
                continue;
            }

            return Some(FrameRun { start, frames: (end - start) / PAGE_SIZE });
        }
        None
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo test -p boot_info'
```

Expected: `test result: ok. 7 passed; 0 failed`.

- [ ] **Step 5: Fix the stale milestone number in the same file**

`boot_info/src/lib.rs` has a comment on `MemoryRegion::kind` reading "Milestone 5's frame allocator cares about `7` (`CONVENTIONAL`)". The renumbering during Milestone 5 left this pointing at the wrong milestone. Change `Milestone 5's` to `Milestone 6's`.

- [ ] **Step 6: Verify nothing else regressed**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo test -p xtask && cargo xtask run'
```

Expected: 3 xtask tests pass; `cargo xtask run` still ends with `PASS: bootloader exited with expected code 33, and the screen has text`. Nothing in this task touches the kernel, so a failure here means something unrelated is broken — stop and investigate rather than continuing.

- [ ] **Step 7: Commit**

```bash
git add boot_info/src/lib.rs
git commit -m "Add UsableRegions, the testable half of the frame allocator

Turns the raw UEFI memory map into page-aligned frame runs: skips
non-conventional regions, clamps unaligned spans inward, drops spans
that round away to nothing, and never yields the frame at physical 0.

Kept in boot_info as pure arithmetic so it host-tests, leaving the
kernel-side allocator with only the pointer work.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: The physical frame allocator

Bump allocation over the runs from Task 1, plus an intrusive free stack whose nodes live inside the free frames themselves. Verified by an in-kernel selftest, because a raw-pointer allocator cannot be exercised on the host.

**Files:**
- Create: `kernel/src/frame.rs`
- Modify: `kernel/src/main.rs` (add `mod frame;`, widen `BootInfo` lifetime, call `frame::init` and `frame::selftest`)

**Interfaces:**
- Consumes: `boot_info::{FrameRun, MemoryRegion, UsableRegions, PAGE_SIZE}` (Task 1); `crate::interrupts::without_interrupts` (exists).
- Produces:
  - `pub fn frame::init(regions: &'static [MemoryRegion]) -> u64` — returns total usable frames; `0` means the map was unusable.
  - `pub fn frame::alloc_frame() -> Option<u64>`
  - `pub fn frame::dealloc_frame(addr: u64)`
  - `pub fn frame::alloc_contiguous(frames: u64) -> Option<u64>`
  - `pub fn frame::selftest() -> Result<(), &'static str>`

- [ ] **Step 1: Widen the `BootInfo` lifetime so the region slice is `'static`**

The allocator outlives `kernel_main`'s stack frame, so it needs `&'static [MemoryRegion]`. The reference is already effectively static — it points at `LOADER_DATA` memory that nothing reclaims — so the honest fix is to say so at the point the reference is created from the raw pointer, rather than transmuting a lifetime later.

In `kernel/src/main.rs`, change the binding in `_start`:

```rust
    // `'static` is correct rather than convenient: this points at memory
    // the bootloader allocated as LOADER_DATA, which survives
    // ExitBootServices and is never reclaimed. Saying so here, where the
    // reference is created from the raw pointer, avoids a lifetime
    // transmute at the frame allocator's door.
    let info: &'static BootInfo = unsafe { &*boot_info };
```

and change `kernel_main`'s signature:

```rust
fn kernel_main(info: &'static BootInfo) -> ! {
```

- [ ] **Step 2: Write the failing selftest wiring**

Add `mod frame;` to the module list in `kernel/src/main.rs` (alphabetical: after `mod font;`, before `mod gdt;`).

Create `kernel/src/frame.rs` with the API present but unimplemented, so the selftest compiles and fails at runtime rather than failing to build:

```rust
//! Physical frame allocator.

use boot_info::{MemoryRegion, PAGE_SIZE};

pub fn init(_regions: &'static [MemoryRegion]) -> u64 {
    0
}

pub fn alloc_frame() -> Option<u64> {
    None
}

pub fn dealloc_frame(_addr: u64) {}

pub fn alloc_contiguous(_frames: u64) -> Option<u64> {
    None
}

pub fn selftest() -> Result<(), &'static str> {
    let first = alloc_frame().ok_or("alloc_frame returned None on an empty allocator")?;
    if first % PAGE_SIZE != 0 {
        return Err("allocated frame is not page aligned");
    }
    if first == 0 {
        return Err("allocated frame is the null frame");
    }

    // Freeing then reallocating must hand the same frame straight back:
    // that is the whole observable behaviour of the intrusive free stack.
    dealloc_frame(first);
    let again = alloc_frame().ok_or("alloc_frame returned None after a dealloc")?;
    if again != first {
        return Err("a freed frame was not the next frame handed out");
    }
    dealloc_frame(again);

    Ok(())
}
```

In `kernel_main`, immediately after the `kprintln!("framebuffer painted")` block and its two `note:` lines, insert:

```rust
    let total_frames = frame::init(unsafe { info.memory_regions() });
    if total_frames == 0 {
        kprintln!("FATAL: no usable memory regions in the UEFI memory map");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }
    kprintln!(
        "frame allocator: {total_frames} usable frames ({} MiB)",
        total_frames * PAGE_SIZE / (1024 * 1024)
    );
    if let Err(reason) = frame::selftest() {
        kprintln!("FATAL: frame allocator selftest: {reason}");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }
    kprintln!("frame allocator: selftest passed");
```

Add `PAGE_SIZE` to the `boot_info` import at the top of `main.rs`:

```rust
use boot_info::{BootInfo, PixelFormatKind, PAGE_SIZE};
```

- [ ] **Step 3: Run to verify it fails for the right reason**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo xtask run'
```

Expected: the trace reaches `framebuffer painted`, then `FATAL: no usable memory regions in the UEFI memory map`, and xtask reports `FAIL: expected exit code 33, got 35`. This confirms the wiring runs at the right point in boot before any of the allocator exists.

- [ ] **Step 4: Write the implementation**

Replace the whole of `kernel/src/frame.rs`:

```rust
//! Physical frame allocator.
//!
//! Two sources, checked in this order: an intrusive stack of frames that
//! were freed, then a bump cursor walking the usable runs of the UEFI
//! memory map. Nothing else is needed at this scope — the kernel is
//! single-core, allocates a fixed heap once at boot, and never returns
//! memory to firmware.
//!
//! The free stack stores each node *inside the free frame itself*, so the
//! allocator carries no side table and its memory cost is exactly zero
//! bytes per free frame. That trick is only sound because the kernel runs
//! on the identity mapping UEFI left in place (see `kernel.ld`), which
//! makes a physical frame address directly writable.

use core::ptr::NonNull;

use boot_info::{MemoryRegion, UsableRegions, PAGE_SIZE};

use crate::interrupts;

/// A free frame's first eight bytes, reinterpreted as a stack node.
struct FreeFrame {
    next: Option<NonNull<FreeFrame>>,
}

struct FrameAllocator {
    /// Runs not yet reached by the bump cursor.
    runs: UsableRegions<'static>,
    /// Next unhanded-out address in the current run.
    cursor: u64,
    /// One past the last usable address in the current run.
    run_end: u64,
    free: Option<NonNull<FreeFrame>>,
    total: u64,
    in_use: u64,
}

/// `static mut` rather than a lock, for the reason `idt.rs` already gives:
/// this kernel is single-core, so the only thing that can interrupt a
/// sequence of instructions is an interrupt, and `without_interrupts` is
/// therefore a complete critical section. Zero-initialised in `.bss`,
/// which `bootloader/src/elf.rs` zeroes before the kernel runs.
static mut FRAMES: Option<FrameAllocator> = None;

impl FrameAllocator {
    fn alloc(&mut self) -> Option<u64> {
        if let Some(node) = self.free {
            // SAFETY: every node on this stack was written by `push_free`
            // into a frame this allocator owns and nothing else
            // references, and is still mapped by the identity mapping.
            self.free = unsafe { node.as_ref().next };
            self.in_use += 1;
            return Some(node.as_ptr() as u64);
        }

        let addr = self.bump(1)?;
        self.in_use += 1;
        Some(addr)
    }

    /// Take `frames` contiguous frames from the bump cursor.
    ///
    /// When the current run is too small, its remaining frames are pushed
    /// onto the free stack one at a time before moving on — otherwise a
    /// large contiguous request would silently strand every tail it
    /// skipped over.
    fn bump(&mut self, frames: u64) -> Option<u64> {
        let wanted = frames.checked_mul(PAGE_SIZE)?;
        loop {
            if self.run_end.saturating_sub(self.cursor) >= wanted {
                let addr = self.cursor;
                self.cursor += wanted;
                return Some(addr);
            }

            while self.cursor + PAGE_SIZE <= self.run_end {
                let stranded = self.cursor;
                self.cursor += PAGE_SIZE;
                self.push_free(stranded);
            }

            let run = self.runs.next()?;
            self.cursor = run.start;
            self.run_end = run.start + run.frames * PAGE_SIZE;
        }
    }

    fn push_free(&mut self, addr: u64) {
        let node = addr as *mut FreeFrame;
        // SAFETY: `addr` is a page-aligned frame this allocator owns, no
        // longer handed out to anyone, and directly writable under the
        // identity mapping. Writing the node here is what makes the free
        // stack cost no side metadata.
        unsafe { node.write(FreeFrame { next: self.free }) };
        self.free = NonNull::new(node);
    }
}

/// Run `f` against the allocator, with interrupts disabled.
///
/// Returns `None` before `init` has run. Every public entry point in this
/// module goes through here, so the critical section is impossible to
/// forget at a call site.
fn with<R>(f: impl FnOnce(&mut FrameAllocator) -> R) -> Option<R> {
    interrupts::without_interrupts(|| {
        // SAFETY: single-core kernel with interrupts disabled for the
        // duration, so no other execution context can hold a reference to
        // `FRAMES` while this one does.
        let slot = unsafe { &mut *(&raw mut FRAMES) };
        slot.as_mut().map(f)
    })
}

/// Seed the allocator from the UEFI memory map.
///
/// Returns the total number of usable frames, or `0` if the map contained
/// no usable memory at all — which the caller must treat as fatal.
pub fn init(regions: &'static [MemoryRegion]) -> u64 {
    let total: u64 = UsableRegions::new(regions).map(|run| run.frames).sum();
    if total == 0 {
        return 0;
    }

    let mut runs = UsableRegions::new(regions);
    // `total > 0` guarantees at least one run, so this cannot be `None`;
    // written as a match anyway so a future change to the guard above
    // cannot turn this into a panic with no logger to report it.
    let (cursor, run_end) = match runs.next() {
        Some(run) => (run.start, run.start + run.frames * PAGE_SIZE),
        None => return 0,
    };

    let allocator = FrameAllocator { runs, cursor, run_end, free: None, total, in_use: 0 };

    interrupts::without_interrupts(|| {
        // SAFETY: called once, from `kernel_main`, before interrupts are
        // enabled; nothing else can hold a reference to `FRAMES`.
        unsafe { (&raw mut FRAMES).write(Some(allocator)) };
    });

    total
}

/// Hand out one 4 KiB frame, or `None` when memory is exhausted.
pub fn alloc_frame() -> Option<u64> {
    with(|allocator| allocator.alloc()).flatten()
}

/// Return a frame previously handed out by [`alloc_frame`].
pub fn dealloc_frame(addr: u64) {
    with(|allocator| {
        allocator.push_free(addr);
        allocator.in_use = allocator.in_use.saturating_sub(1);
    });
}

/// Hand out `frames` *contiguous* frames.
///
/// Served from the bump cursor only: a LIFO free stack cannot promise
/// adjacency. The heap's one-time 1 MiB request is the only caller.
pub fn alloc_contiguous(frames: u64) -> Option<u64> {
    with(|allocator| {
        let addr = allocator.bump(frames)?;
        allocator.in_use += frames;
        Some(addr)
    })
    .flatten()
}

/// Boot-time proof that both allocation paths work.
///
/// Returns the name of the violated invariant rather than panicking, so
/// the caller can report it on serial and exit with the failure code —
/// the same shape the keyboard check in `kernel_main` already uses.
pub fn selftest() -> Result<(), &'static str> {
    let first = alloc_frame().ok_or("alloc_frame returned None on a fresh allocator")?;
    if first % PAGE_SIZE != 0 {
        return Err("allocated frame is not page aligned");
    }
    if first == 0 {
        return Err("allocated frame is the null frame");
    }

    // Freeing then reallocating must hand the same frame straight back:
    // that is the whole observable behaviour of the intrusive free stack,
    // and it fails loudly if the in-frame node write went wrong.
    dealloc_frame(first);
    let again = alloc_frame().ok_or("alloc_frame returned None after a dealloc")?;
    if again != first {
        return Err("a freed frame was not the next frame handed out");
    }
    dealloc_frame(again);

    // A second, distinct frame must also be available, so a broken bump
    // cursor that only ever returns one address is caught here.
    let a = alloc_frame().ok_or("alloc_frame returned None for the first of two frames")?;
    let b = alloc_frame().ok_or("alloc_frame returned None for the second of two frames")?;
    if a == b {
        return Err("two consecutive allocations returned the same frame");
    }
    dealloc_frame(b);
    dealloc_frame(a);

    Ok(())
}
```

- [ ] **Step 5: Run to verify it passes**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo xtask run'
```

Expected: the trace now contains

```
frame allocator: N usable frames (M MiB)
frame allocator: selftest passed
```

with `N` in the tens of thousands (QEMU is started with `-m 256M`), followed by the unchanged timer and keyboard lines and `PASS: bootloader exited with expected code 33, and the screen has text`.

If the glyph assertion fails here, the allocator init was inserted before `console::init` — move it after.

- [ ] **Step 6: Verify the rest of the suite**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo test -p boot_info && cargo test -p xtask && cargo xtask test'
```

Expected: 7 pass, 3 pass, and `PASS: bootloader rejected the corrupted image (exit 35)`.

- [ ] **Step 7: Commit**

```bash
git add kernel/src/frame.rs kernel/src/main.rs
git commit -m "Add the physical frame allocator

Bump-allocates 4 KiB frames over the usable runs of the UEFI memory map,
with an intrusive free stack for reclaimed frames whose nodes live inside
the free frames themselves — so the allocator carries no side metadata.
Sound only because the kernel runs on UEFI's identity mapping, which the
module header records.

Frames stranded at the tail of a run by a contiguous request are pushed
onto the free stack rather than leaked. A boot-time selftest proves both
paths and exits with the failure code if either is wrong.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: The kernel heap and `#[global_allocator]`

An address-ordered free list with splitting and bidirectional coalescing, over 256 contiguous frames from Task 2.

**The 16-byte grid.** Every block start and every block size is a multiple of 16. The heap's backing memory is frame-aligned, so the first block satisfies it; every requested size is rounded up to a multiple of 16; every requested alignment is raised to at least 16. Because a power-of-two alignment of 16 or more keeps a 16-aligned start 16-aligned, the front padding and the tail remainder of any carve are themselves multiples of 16 — so each is either exactly 0 or at least 16, which is a whole `FreeBlock`. This is what makes the awkward case structurally impossible instead of something to handle: there is never a remainder too small to hold a header, so no allocation ever has to absorb a prefix it could not later recover from a payload pointer.

**Files:**
- Create: `kernel/src/heap.rs`
- Modify: `kernel/src/main.rs` (add `extern crate alloc;` and `mod heap;`, call `heap::init` and `heap::selftest`)
- Modify: `docs/superpowers/specs/2026-08-27-frame-heap-allocator-design.md` (correct the front-padding paragraph)

**Interfaces:**
- Consumes: `crate::frame::alloc_contiguous` (Task 2); `crate::interrupts::without_interrupts`; `boot_info::PAGE_SIZE`.
- Produces:
  - `pub const heap::HEAP_FRAMES: u64 = 256;`
  - `pub fn heap::init() -> Option<(u64, usize)>` — `(start address, size in bytes)`
  - `pub fn heap::free_bytes() -> usize`
  - `pub fn heap::selftest() -> Result<(), &'static str>`
  - `#[global_allocator] static ALLOCATOR: KernelHeap`

- [ ] **Step 1: Write the failing wiring**

Add to the very top of `kernel/src/main.rs`, after the `#![feature(abi_x86_interrupt)]` line:

```rust
extern crate alloc;
```

Add `mod heap;` to the module list (after `mod gdt;`, before `mod idt;`).

Create `kernel/src/heap.rs` with the API present but unimplemented:

```rust
//! The kernel heap.

pub const HEAP_FRAMES: u64 = 256;

pub fn init() -> Option<(u64, usize)> {
    None
}

pub fn free_bytes() -> usize {
    0
}

pub fn selftest() -> Result<(), &'static str> {
    Err("heap is not implemented yet")
}
```

In `kernel_main`, immediately after the `frame allocator: selftest passed` line, insert:

```rust
    let Some((heap_start, heap_size)) = heap::init() else {
        kprintln!("FATAL: could not obtain {HEAP_FRAMES} contiguous frames for the heap");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    };
    kprintln!(
        "heap: {} KiB @ {heap_start:#x} ({HEAP_FRAMES} frames)",
        heap_size / 1024
    );
    if let Err(reason) = heap::selftest() {
        kprintln!("FATAL: heap selftest: {reason}");
        qemu_exit::exit(qemu_exit::QemuExitCode::Failed);
    }
    kprintln!("alloc: Box/Vec/String OK, heap balanced");
```

Add the import near the top of `main.rs`:

```rust
use heap::HEAP_FRAMES;
```

- [ ] **Step 2: Run to verify it fails for the right reason**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo xtask run'
```

Expected: the trace reaches `frame allocator: selftest passed`, then `FATAL: could not obtain 256 contiguous frames for the heap`, and xtask reports `FAIL: expected exit code 33, got 35`.

- [ ] **Step 3: Write the implementation**

Replace the whole of `kernel/src/heap.rs`:

```rust
//! The kernel heap: an address-ordered free list with splitting and
//! bidirectional coalescing, backed by frames from `crate::frame`.
//!
//! Each free block stores its own header in its first sixteen bytes, so
//! like the frame allocator this costs no side metadata. Unlike the frame
//! allocator, freeing must *merge* — `Vec` grows by repeated reallocation,
//! and without coalescing the heap would fragment into a chain of
//! unusable slivers within a few pushes.
//!
//! # The 16-byte grid
//!
//! Every block start and every block size is a multiple of [`GRAIN`].
//! The backing memory is frame-aligned, sizes are rounded up, and
//! alignments are raised to at least `GRAIN` — and a power-of-two
//! alignment of 16 or more maps a 16-aligned address to a 16-aligned
//! address. So the front padding and tail remainder of any carve are also
//! multiples of 16: either exactly zero, or big enough to be a whole
//! `FreeBlock`. The case of "a remainder too small to hold a header" —
//! which would otherwise force an allocation to swallow a prefix it could
//! never recover from a payload pointer at `dealloc` time — cannot arise.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use boot_info::PAGE_SIZE;

use crate::frame;
use crate::interrupts;

/// 1 MiB. Ample for the selftest and for Milestone 7's keyboard
/// buffering, and well under a percent of the 256 MiB `xtask` boots QEMU
/// with. If the target's memory is ever reduced, this is the constant to
/// revisit — the failure is loud, at `init`, not silent.
pub const HEAP_FRAMES: u64 = 256;

/// The granularity every block start and size is a multiple of. Equal to
/// `size_of::<FreeBlock>()`; see the module header for why this matters.
const GRAIN: usize = 16;

/// A free block's header, stored in the block's own first bytes.
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const _: () = assert!(size_of::<FreeBlock>() == GRAIN);

struct Heap {
    head: *mut FreeBlock,
}

/// `static mut` for the same reason as `FRAMES` in `frame.rs`: single
/// core, so `without_interrupts` is a complete critical section.
static mut HEAP: Heap = Heap { head: ptr::null_mut() };

/// Round a request up onto the grid, and raise its alignment onto it too.
fn normalise(layout: Layout) -> (usize, usize) {
    let size = layout.size().max(GRAIN).next_multiple_of(GRAIN);
    let align = layout.align().max(GRAIN);
    (size, align)
}

impl Heap {
    /// # Safety
    /// `start` must be a live, frame-aligned region of `size` bytes that
    /// nothing else uses for the lifetime of the kernel.
    unsafe fn init(&mut self, start: u64, size: usize) {
        let block = start as *mut FreeBlock;
        unsafe { block.write(FreeBlock { size, next: ptr::null_mut() }) };
        self.head = block;
    }

    /// # Safety
    /// Standard `GlobalAlloc::alloc` contract.
    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let (size, align) = normalise(layout);

        let mut prev: *mut FreeBlock = ptr::null_mut();
        let mut cur = self.head;

        while !cur.is_null() {
            // SAFETY: every block on this list was written by `init`,
            // `split_off`, or `dealloc`, and is still owned by the heap.
            let block_size = unsafe { (*cur).size };
            let next = unsafe { (*cur).next };
            let block_start = cur as usize;

            let payload = block_start.next_multiple_of(align);
            let front = payload - block_start;

            // Both `front` and the tail are multiples of GRAIN, so each is
            // either 0 or a whole block — see the module header.
            if block_size >= front + size {
                let tail = block_size - front - size;

                let remainder = if tail >= GRAIN {
                    let tail_block = (payload + size) as *mut FreeBlock;
                    // SAFETY: `tail_block` lies inside this block, is
                    // GRAIN-aligned, and holds at least GRAIN bytes.
                    unsafe { tail_block.write(FreeBlock { size: tail, next }) };
                    tail_block
                } else {
                    next
                };

                if front >= GRAIN {
                    // Keep the front as its own free block, shrunk to fit.
                    // SAFETY: `cur` is a live block header we own.
                    unsafe {
                        (*cur).size = front;
                        (*cur).next = remainder;
                    }
                } else if prev.is_null() {
                    self.head = remainder;
                } else {
                    // SAFETY: `prev` is a live block header we own.
                    unsafe { (*prev).next = remainder };
                }

                return payload as *mut u8;
            }

            prev = cur;
            cur = next;
        }

        // Out of memory. Returning null is the contract; the default OOM
        // handler turns it into a panic, which this kernel's panic handler
        // reports on serial before exiting with the failure code.
        ptr::null_mut()
    }

    /// # Safety
    /// `ptr` must have come from [`Self::alloc`] with the same `layout`.
    unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let (size, _) = normalise(layout);
        let start = ptr as usize;

        // Find the insertion point, keeping the block that precedes it.
        let mut prev: *mut FreeBlock = ptr::null_mut();
        let mut cur = self.head;
        while !cur.is_null() && (cur as usize) < start {
            prev = cur;
            // SAFETY: `cur` is a live block header we own.
            cur = unsafe { (*cur).next };
        }

        // Merge forward first: if the block being freed runs exactly up to
        // the next free block, the two become one.
        let (mut size, mut next) = (size, cur);
        if !cur.is_null() && start + size == cur as usize {
            // SAFETY: `cur` is a live block header we own.
            size += unsafe { (*cur).size };
            next = unsafe { (*cur).next };
        }

        // Then merge backward: if the previous free block runs exactly up
        // to the block being freed, grow it instead of linking a new one.
        if !prev.is_null() {
            // SAFETY: `prev` is a live block header we own.
            let prev_end = prev as usize + unsafe { (*prev).size };
            if prev_end == start {
                unsafe {
                    (*prev).size += size;
                    (*prev).next = next;
                }
                return;
            }
        }

        let block = start as *mut FreeBlock;
        // SAFETY: `start` is GRAIN-aligned and owns at least GRAIN bytes,
        // and is no longer referenced by the caller.
        unsafe { block.write(FreeBlock { size, next }) };

        if prev.is_null() {
            self.head = block;
        } else {
            // SAFETY: `prev` is a live block header we own.
            unsafe { (*prev).next = block };
        }
    }

    fn free_bytes(&self) -> usize {
        let mut total = 0;
        let mut cur = self.head;
        while !cur.is_null() {
            // SAFETY: `cur` is a live block header we own.
            unsafe {
                total += (*cur).size;
                cur = (*cur).next;
            }
        }
        total
    }
}

/// The `#[global_allocator]`.
///
/// A unit struct, so it is `Sync` without any unsafe assertion; all the
/// shared state lives in `HEAP`, and every access to that goes through
/// `without_interrupts`. On a single-core kernel that is a complete
/// critical section, which is what makes it safe for an interrupt handler
/// to allocate without corrupting a list walk already in progress.
pub struct KernelHeap;

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        interrupts::without_interrupts(|| {
            // SAFETY: interrupts are disabled and the kernel is
            // single-core, so no other context holds this reference.
            let heap = unsafe { &mut *(&raw mut HEAP) };
            unsafe { heap.alloc(layout) }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        interrupts::without_interrupts(|| {
            // SAFETY: as above.
            let heap = unsafe { &mut *(&raw mut HEAP) };
            unsafe { heap.dealloc(ptr, layout) }
        })
    }
}

#[global_allocator]
static ALLOCATOR: KernelHeap = KernelHeap;

/// Claim the heap's backing frames and arm the allocator.
///
/// Returns `(start, size)`, or `None` if no run of [`HEAP_FRAMES`]
/// contiguous frames was available — which the caller must treat as fatal.
pub fn init() -> Option<(u64, usize)> {
    let start = frame::alloc_contiguous(HEAP_FRAMES)?;
    let size = (HEAP_FRAMES * PAGE_SIZE) as usize;

    interrupts::without_interrupts(|| {
        // SAFETY: called once, before interrupts are enabled, with a
        // region the frame allocator has just handed out exclusively.
        let heap = unsafe { &mut *(&raw mut HEAP) };
        unsafe { heap.init(start, size) };
    });

    Some((start, size))
}

/// Total free bytes across the list. Used by the selftest to prove that
/// freeing everything really does restore the heap.
pub fn free_bytes() -> usize {
    interrupts::without_interrupts(|| {
        // SAFETY: as in `KernelHeap::alloc`.
        let heap = unsafe { &*(&raw const HEAP) };
        heap.free_bytes()
    })
}
```

- [ ] **Step 4: Write the heap selftest**

Append to `kernel/src/heap.rs`:

```rust
/// Boot-time proof that `alloc` works and that freeing restores the heap.
///
/// The free-byte balance at the end is the real assertion: it only returns
/// to its starting value if coalescing merges in both directions. A heap
/// that splits correctly but never merges passes every other check here
/// and fails this one.
pub fn selftest() -> Result<(), &'static str> {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    let before = free_bytes();

    {
        let boxed = Box::new(0xdead_beef_u64);
        if *boxed != 0xdead_beef {
            return Err("Box did not round-trip its value");
        }
    }

    {
        // 1024 pushes from empty forces several reallocations, which is
        // the allocation pattern most likely to expose a split or merge
        // bug.
        let mut values: Vec<u64> = Vec::new();
        for i in 0..1024 {
            values.push(i);
        }
        if values.len() != 1024 {
            return Err("Vec length is wrong after growth");
        }
        if values[0] != 0 || values[1023] != 1023 {
            return Err("Vec contents are wrong after reallocation");
        }
    }

    {
        let mut text = String::new();
        for _ in 0..100 {
            text.push_str("rust");
        }
        if text.len() != 400 {
            return Err("String length is wrong after growth");
        }
    }

    {
        // Free three adjacent allocations middle-last, so the final free
        // has a free neighbour on both sides and must merge both ways.
        let first = Vec::<u8>::with_capacity(64);
        let middle = Vec::<u8>::with_capacity(64);
        let last = Vec::<u8>::with_capacity(64);
        drop(first);
        drop(last);
        drop(middle);
    }

    let after = free_bytes();
    if after != before {
        return Err("heap did not return to its starting free-byte count");
    }

    Ok(())
}
```

- [ ] **Step 5: Run to verify it passes**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo xtask run'
```

Expected trace additions:

```
heap: 1024 KiB @ 0x... (256 frames)
alloc: Box/Vec/String OK, heap balanced
```

then the unchanged timer/keyboard lines and `PASS: bootloader exited with expected code 33, and the screen has text`.

If it reports `heap did not return to its starting free-byte count`, coalescing is wrong — that is the check doing its job. Debug it before continuing; do not weaken the assertion.

- [ ] **Step 6: Correct the front-padding paragraph in the spec**

The spec describes front padding as "absorbed into the allocation" when smaller than 16 bytes. That is unimplementable without a per-allocation header, because `dealloc` receives only the payload pointer and layout and could never recover the absorbed prefix. Replace that sentence in `docs/superpowers/specs/2026-08-27-frame-heap-allocator-design.md` with a description of the 16-byte grid, matching the module header of `heap.rs`.

- [ ] **Step 7: Verify the full suite**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo test -p boot_info && cargo test -p xtask && cargo xtask run && cargo xtask test'
```

Expected: 7 pass, 3 pass, exit-33 PASS, exit-35 PASS.

- [ ] **Step 8: Commit**

```bash
git add kernel/src/heap.rs kernel/src/main.rs docs/superpowers/specs/2026-08-27-frame-heap-allocator-design.md
git commit -m "Add the kernel heap and register it as the global allocator

An address-ordered free list over 256 contiguous frames, with splitting
and bidirectional coalescing. Block headers live inside the free blocks,
so like the frame allocator it costs no side metadata.

Every block start and size sits on a 16-byte grid, which makes a
remainder too small to hold a header structurally impossible. This
replaces the spec's 'absorb small front padding' rule, which could not
have worked: dealloc sees only the payload pointer and could never
recover an absorbed prefix.

The selftest's free-byte balance check is the real assertion — a heap
that splits but never merges passes everything else and fails that.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: Documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/design.md`
- Modify: `kernel/src/interrupts.rs` (stale milestone number)
- Create: `docs/plans/milestone-6-frame-allocator.md`

**Interfaces:** none — documentation only.

- [ ] **Step 1: Fix the stale milestone number in `interrupts.rs`**

The `without_interrupts` comment says "Milestone 5's frame allocator and heap will protect genuinely multi-word state with exactly this function". The renumbering left this wrong, and the tense is now stale too. Change it to read that Milestone 6's frame allocator and heap *do* protect multi-word state with this function — it is no longer a prediction.

- [ ] **Step 2: Update `README.md`**

Three edits:

1. Roadmap: change `- [ ] 6. Kernel: physical frame allocator + heap allocator` to `- [x] 6. Kernel: physical frame allocator + heap allocator`.
2. Status paragraph: rewrite for Milestone 6 — the kernel now manages its own physical memory and has a working heap, so `Box`, `Vec`, and `String` are available; mention that the frame allocator is seeded from the UEFI memory map and the heap is carved from it.
3. The stack guard page sentence near the end of the "Build & run" section currently promises `Milestone 6's stack guard page will make a genuine stack-overflow double fault happen naturally`. Milestone 6 did not add paging. Rewrite it to say the double-fault handler remains installed but untested-by-design, and that making it fire naturally needs a guard page, which needs page-table work listed as future work in `docs/design.md`.

Also add the two new trace lines to the sample kernel trace block, in their real position — after `framebuffer painted` and before `enabling interrupts`:

```
frame allocator: 64512 usable frames (252 MiB)
frame allocator: selftest passed
heap: 1024 KiB @ 0x... (256 frames)
alloc: Box/Vec/String OK, heap balanced
```

Use the real numbers from the actual run rather than these placeholders.

- [ ] **Step 3: Update `docs/design.md`**

Two edits:

1. In "Open questions / decisions deferred to implementation", strike through the hand-rolled-vs-`linked_list_allocator` question the way the other resolved questions in that file are struck through, and record the resolution: both allocators hand-rolled, no new dependency, for the learning and portfolio value that is the project's stated purpose.
2. In "Memory management (MVP scope)", replace the speculative description with what was built: a bump allocator with an intrusive free stack over the map's conventional regions, and a 1 MiB address-ordered coalescing free-list heap carved from it. Keep the existing "No custom page tables in MVP" paragraph as-is — it is still true, and Milestone 6 deliberately did not change it.

- [ ] **Step 4: Write `docs/plans/milestone-6-frame-allocator.md`**

Match the shape of the existing per-milestone plans in `docs/plans/`. Read `docs/plans/milestone-5-framebuffer-text.md` first and follow its structure. It must record, at minimum:

- What was built and why the two layers are separate.
- Why both allocators are hand-rolled.
- The identity-mapping premise the intrusive free lists rest on.
- The 16-byte grid, and the front-padding problem it makes impossible.
- Why the frame allocator's region arithmetic lives in `boot_info` (so it host-tests).
- The two init-order constraints, including the non-obvious one: initialising before `console::init` changes the first on-screen glyph and fails `xtask`'s pixel assertion with an error that mentions only glyphs.
- What was deliberately not built: paging, the stack guard page, and freeing the heap's backing frames.

- [ ] **Step 5: Verify the suite one final time**

```bash
wsl -e bash -lc 'cd /home/coolg/projects/Rust_BL && cargo test -p boot_info && cargo test -p xtask && cargo xtask run && cargo xtask test'
```

Expected: 7 pass, 3 pass, exit-33 PASS, exit-35 PASS.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/design.md docs/plans/milestone-6-frame-allocator.md kernel/src/interrupts.rs
git commit -m "Document Milestone 6 and correct the guard-page promise

Ticks the roadmap, records the resolved hand-rolled-vs-crate decision in
the design spec, and adds the per-milestone plan.

Also corrects two leftovers from the Milestone 5 renumbering: the stale
'Milestone 5's frame allocator' comment in interrupts.rs, and the README
claim that Milestone 6 would add a stack guard page. It did not — that
needs page tables, which design.md keeps out of MVP scope.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Self-review

**Spec coverage.** Every spec section maps to a task: `UsableRegions` and its five stated rules to Task 1; `frame.rs` with bump, intrusive free stack, and `alloc_contiguous` to Task 2; `heap.rs`, the `#[global_allocator]`, and heap sizing to Task 3; all four error-handling rows to the fatal branches in Tasks 2 and 3 plus the no-new-code OOM path; the three testing layers to Task 1 Step 1, Task 3 Step 4, and the regression commands ending every task; all five documentation changes to Task 1 Step 5, Task 3 Step 6, and Task 4.

**Two deviations from the spec, both deliberate:**

1. **Front padding.** The spec's "absorbed into the allocation" rule cannot work — `dealloc` receives only the payload pointer and layout, so an absorbed prefix is unrecoverable. Replaced by the 16-byte grid, which makes the case impossible. Task 3 Step 6 corrects the spec.
2. **`unsafe impl Sync`.** The spec called for one on the global allocator. Not needed: `KernelHeap` is a unit struct and therefore already `Sync`. The shared state is the `static mut HEAP`, guarded by `without_interrupts`. The weaker, honest construct wins.

**Placeholder scan.** No TBD/TODO. Every code step carries real code. The one intentionally deferred value is the sample trace numbers in Task 4 Step 2, which are explicitly marked to be replaced with real output.

**Type consistency.** `FrameRun { start, frames }` is constructed in Task 1 and consumed in Task 2's `bump`/`init`. `PAGE_SIZE` is defined once in `boot_info` and imported by `frame.rs`, `heap.rs`, and `main.rs`. `HEAP_FRAMES` is defined in `heap.rs` and imported by `main.rs`. `selftest() -> Result<(), &'static str>` has the same signature in both modules and the same call shape at both call sites. `alloc_contiguous(frames: u64) -> Option<u64>` is produced in Task 2 and consumed in Task 3 with matching types.
