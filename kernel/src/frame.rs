//! Physical frame allocator.
//!
//! Two sources, checked in this order: an intrusive stack of frames that
//! were freed, then a bump cursor walking the usable runs of the UEFI
//! memory map. Nothing more is needed at this scope — the kernel is
//! single-core, allocates a fixed heap once at boot, and never returns
//! memory to firmware.
//!
//! The free stack stores each node *inside the free frame itself*, so the
//! allocator carries no side table: a free frame costs exactly zero bytes
//! of bookkeeping. That trick is only sound because the kernel runs on the
//! identity mapping UEFI left in place (see the header comment in
//! `kernel.ld`), which makes a physical frame address directly writable.
//!
//! Deliberately not a bitmap allocator: a bitmap needs storage
//! proportional to RAM, and that storage has to be found *before* any
//! allocator exists. The bump-plus-free-stack shape has no such bootstrap
//! problem.

use core::ptr::NonNull;

use boot_info::{MemoryRegion, UsableRegions, PAGE_SIZE};

use crate::interrupts;

/// A free frame's first eight bytes, reinterpreted as a stack node.
struct FreeFrame {
    next: Option<NonNull<FreeFrame>>,
}

struct FrameAllocator {
    /// Runs the bump cursor has not reached yet.
    runs: UsableRegions<'static>,
    /// Next address in the current run that has not been handed out.
    cursor: u64,
    /// One past the last usable address in the current run.
    run_end: u64,
    free: Option<NonNull<FreeFrame>>,
    /// Frames handed out and not returned. Reported by the heap's trace
    /// line; the total is not stored, since `init` returns it and only
    /// `kernel_main` ever wanted it.
    in_use: u64,
}

/// `static mut` rather than a lock, for the reason `idt.rs` already spells
/// out: this kernel is single-core, so the only thing that can interrupt a
/// sequence of instructions is an interrupt, and `without_interrupts` is
/// therefore a complete critical section. Zero-initialised in `.bss`,
/// which `bootloader/src/elf.rs` zeroes before the kernel runs.
static mut FRAMES: Option<FrameAllocator> = None;

impl FrameAllocator {
    fn alloc(&mut self) -> Option<u64> {
        if let Some(node) = self.free {
            // SAFETY: every node on this stack was written by `push_free`
            // into a frame this allocator owns, that nothing else
            // references, and that is still covered by the identity
            // mapping.
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
    /// When the current run is too small to satisfy the request, its
    /// remaining frames are pushed onto the free stack one at a time
    /// before moving on. Without that, a large contiguous request would
    /// silently strand every run tail it stepped over — invisible, and
    /// unrecoverable for the rest of the kernel's life.
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
        // identity mapping. Writing the node into the frame itself is what
        // makes the free stack cost no side metadata.
        unsafe { node.write(FreeFrame { next: self.free }) };
        self.free = NonNull::new(node);
    }
}

/// Run `f` against the allocator with interrupts disabled.
///
/// Returns `None` before `init` has run. Every public entry point in this
/// module goes through here, so the critical section cannot be forgotten
/// at an individual call site.
fn with<R>(f: impl FnOnce(&mut FrameAllocator) -> R) -> Option<R> {
    interrupts::without_interrupts(|| {
        // SAFETY: single-core kernel with interrupts disabled for the
        // whole closure, so no other execution context can hold a
        // reference to `FRAMES` while this one does.
        let slot = unsafe { &mut *(&raw mut FRAMES) };
        slot.as_mut().map(f)
    })
}

/// Seed the allocator from the UEFI memory map.
///
/// Returns the total number of usable frames, or `0` if the map held no
/// usable memory at all — which the caller must treat as fatal.
pub fn init(regions: &'static [MemoryRegion]) -> u64 {
    let total: u64 = UsableRegions::new(regions).map(|run| run.frames).sum();
    if total == 0 {
        return 0;
    }

    let mut runs = UsableRegions::new(regions);
    // `total > 0` guarantees at least one run, so this cannot be `None`.
    // Written as a match anyway rather than an unwrap, so that a future
    // change to the guard above degrades into the caller's fatal-error
    // path instead of a panic with no fault handler to report it.
    let (cursor, run_end) = match runs.next() {
        Some(run) => (run.start, run.start + run.frames * PAGE_SIZE),
        None => return 0,
    };

    let allocator = FrameAllocator { runs, cursor, run_end, free: None, in_use: 0 };

    interrupts::without_interrupts(|| {
        // SAFETY: called once, from `kernel_main`, before interrupts are
        // enabled, so nothing else can hold a reference to `FRAMES`.
        unsafe { (&raw mut FRAMES).write(Some(allocator)) };
    });

    total
}

/// Hand out one 4 KiB frame, or `None` when physical memory is exhausted.
pub fn alloc_frame() -> Option<u64> {
    with(|allocator| allocator.alloc()).flatten()
}

/// Return a frame previously handed out by [`alloc_frame`].
pub fn dealloc_frame(addr: u64) {
    let _ = with(|allocator| {
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

/// Frames handed out and not returned. Reported by the heap's trace line.
pub fn frames_in_use() -> u64 {
    with(|allocator| allocator.in_use).unwrap_or(0)
}

/// Boot-time proof that both allocation paths work.
///
/// Returns the name of the violated invariant rather than panicking, so
/// the caller can report it on serial and exit with the failure code — the
/// same shape the keyboard check in `kernel_main` already uses.
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
    // and it fails loudly here if the in-frame node write went wrong.
    dealloc_frame(first);
    let again = alloc_frame().ok_or("alloc_frame returned None after a dealloc")?;
    if again != first {
        return Err("a freed frame was not the next frame handed out");
    }
    dealloc_frame(again);

    // Two live allocations must be distinct, so a broken bump cursor that
    // keeps returning one address is caught rather than looking correct.
    let a = alloc_frame().ok_or("alloc_frame returned None for the first of two frames")?;
    let b = alloc_frame().ok_or("alloc_frame returned None for the second of two frames")?;
    if a == b {
        return Err("two consecutive allocations returned the same frame");
    }
    dealloc_frame(b);
    dealloc_frame(a);

    Ok(())
}
