//! The kernel heap: an address-ordered free list with splitting and
//! bidirectional coalescing, backed by frames from [`crate::frame`].
//!
//! Each free block stores its header in its own first sixteen bytes, so
//! like the frame allocator this costs no side metadata. Unlike the frame
//! allocator, freeing must *merge*: `Vec` grows by repeated reallocation,
//! and a heap that split but never merged would fragment into a chain of
//! unusable slivers within a few pushes while still reporting plenty of
//! free bytes.
//!
//! # The 16-byte grid
//!
//! Every block start and every block size is a multiple of [`GRAIN`]. The
//! backing memory is frame-aligned, requested sizes are rounded up, and
//! requested alignments are raised to at least `GRAIN` — and a
//! power-of-two alignment of 16 or more maps a 16-aligned address to a
//! 16-aligned address. So the front padding and the tail remainder of any
//! carve are themselves multiples of 16: each is either exactly zero, or
//! large enough to be a whole `FreeBlock`.
//!
//! That is what makes the awkward case impossible rather than something to
//! handle. A remainder too small to hold a header would force the
//! allocation to swallow a prefix, and `dealloc` — which receives only the
//! payload pointer and the layout — could never recover it. On the grid,
//! that situation cannot arise.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use boot_info::PAGE_SIZE;

use crate::frame;
use crate::interrupts;

/// 1 MiB. Ample for the selftest and for Milestone 7's keyboard
/// buffering, and well under a percent of the 256 MiB `xtask` boots QEMU
/// with. If the target's memory is ever reduced this is the constant to
/// revisit — the failure is loud, at `init`, rather than silent.
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
    /// The standard `GlobalAlloc::alloc` contract.
    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let (size, align) = normalise(layout);

        let mut prev: *mut FreeBlock = ptr::null_mut();
        let mut cur = self.head;

        while !cur.is_null() {
            // SAFETY: every block on this list was written by `init`, by
            // the split below, or by `dealloc`, and is still owned by the
            // heap and unreferenced by anyone else.
            let block_size = unsafe { (*cur).size };
            let next = unsafe { (*cur).next };
            let block_start = cur as usize;

            let payload = block_start.next_multiple_of(align);
            let front = payload - block_start;

            if block_size >= front + size {
                let tail = block_size - front - size;

                // Both `front` and `tail` are multiples of GRAIN, so each
                // is either 0 or a whole block — see the module header.
                let remainder = if tail >= GRAIN {
                    let tail_block = (payload + size) as *mut FreeBlock;
                    // SAFETY: `tail_block` lies inside the block being
                    // carved, is GRAIN-aligned, and spans at least GRAIN
                    // bytes.
                    unsafe { tail_block.write(FreeBlock { size: tail, next }) };
                    tail_block
                } else {
                    next
                };

                if front >= GRAIN {
                    // Keep the front as its own free block, shrunk to fit.
                    // SAFETY: `cur` is a live block header the heap owns.
                    unsafe {
                        (*cur).size = front;
                        (*cur).next = remainder;
                    }
                } else if prev.is_null() {
                    self.head = remainder;
                } else {
                    // SAFETY: `prev` is a live block header the heap owns.
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

        // Find the insertion point, remembering the block before it.
        let mut prev: *mut FreeBlock = core::ptr::null_mut();
        let mut cur = self.head;
        while !cur.is_null() && (cur as usize) < start {
            prev = cur;
            // SAFETY: `cur` is a live block header the heap owns.
            cur = unsafe { (*cur).next };
        }

        // Merge forward: if this block runs exactly up to the next free
        // block, the two become one.
        let (mut size, mut next) = (size, cur);
        if !cur.is_null() && start + size == cur as usize {
            // SAFETY: `cur` is a live block header the heap owns.
            size += unsafe { (*cur).size };
            next = unsafe { (*cur).next };
        }

        // Merge backward: if the previous free block runs exactly up to
        // this one, grow it in place instead of linking a new header.
        if !prev.is_null() {
            // SAFETY: `prev` is a live block header the heap owns.
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
        // SAFETY: `start` is GRAIN-aligned, spans at least GRAIN bytes,
        // and the caller has given up its claim to the allocation.
        unsafe { block.write(FreeBlock { size, next }) };

        if prev.is_null() {
            self.head = block;
        } else {
            // SAFETY: `prev` is a live block header the heap owns.
            unsafe { (*prev).next = block };
        }
    }

    fn free_bytes(&self) -> usize {
        let mut total = 0;
        let mut cur = self.head;
        while !cur.is_null() {
            // SAFETY: `cur` is a live block header the heap owns.
            unsafe {
                total += (*cur).size;
                cur = (*cur).next;
            }
        }
        total
    }

    /// How many separate blocks the free list holds.
    ///
    /// This, not the free *byte* count, is what detects a coalescing bug.
    /// Total free bytes are conserved whether or not neighbours merge — a
    /// heap that never coalesced would fragment into many small blocks
    /// summing to exactly the same total. Only the block count falls back
    /// to one when merging actually happens.
    fn free_blocks(&self) -> usize {
        let mut count = 0;
        let mut cur = self.head;
        while !cur.is_null() {
            count += 1;
            // SAFETY: `cur` is a live block header the heap owns.
            cur = unsafe { (*cur).next };
        }
        count
    }
}

/// The `#[global_allocator]`.
///
/// A unit struct, so it is `Sync` without any unsafe assertion — all the
/// shared state lives in `HEAP`, and every access to that goes through
/// `without_interrupts`. On a single-core kernel that is a complete
/// critical section, which is what makes it safe for a future interrupt
/// handler to allocate without corrupting a list walk already in progress.
pub struct KernelHeap;

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        interrupts::without_interrupts(|| {
            // SAFETY: interrupts are disabled for the whole closure and
            // the kernel is single-core, so no other execution context
            // holds this reference.
            let heap = unsafe { &mut *(&raw mut HEAP) };
            unsafe { heap.alloc(layout) }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        interrupts::without_interrupts(|| {
            // SAFETY: as in `alloc` above.
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

/// Total free bytes across the list. Proves nothing leaked.
pub fn free_bytes() -> usize {
    interrupts::without_interrupts(|| {
        // SAFETY: as in `KernelHeap::alloc`.
        let heap = unsafe { &*(&raw const HEAP) };
        heap.free_bytes()
    })
}

/// Number of blocks on the free list. Proves that freeing coalesced.
pub fn free_blocks() -> usize {
    interrupts::without_interrupts(|| {
        // SAFETY: as in `KernelHeap::alloc`.
        let heap = unsafe { &*(&raw const HEAP) };
        heap.free_blocks()
    })
}

/// Boot-time proof that `alloc` works and that freeing restores the heap.
///
/// Two closing assertions, and they catch different faults. The free-byte
/// balance catches a leak — bytes carved out and never given back. The
/// block count catches a *coalescing* failure, which the byte balance
/// cannot see at all: total free bytes are conserved whether or not
/// neighbours merge, so a heap that split correctly and never merged
/// would fragment into a chain of slivers summing to exactly the right
/// total and pass the balance check. Only the block count returning to
/// one shows that merging really happened.
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
        // the pattern most likely to expose a split or merge bug.
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

    if free_bytes() != before {
        return Err("heap did not return to its starting free-byte count");
    }

    // Everything allocated above has been dropped, so a heap that merges
    // correctly is back to the single block `init` created.
    if free_blocks() != 1 {
        return Err("free list did not coalesce back to a single block");
    }

    Ok(())
}
