# Milestone 6 — Physical Frame Allocator + Kernel Heap

Date: 2026-08-27

## Goal

Give the kernel working `alloc` collection types (`Box`, `Vec`, `String`) on
top of memory the kernel manages itself, rather than a static array. Two
layers, both hand-rolled:

1. A **physical frame allocator** seeded from the UEFI memory map already
   carried in `BootInfo`, handing out 4 KiB frames.
2. A **kernel heap** built on a contiguous run of frames obtained from that
   allocator, registered as the `#[global_allocator]`.

The layering is the point: the frame allocator is load-bearing, not
decorative — the heap cannot exist without it.

## Scope

In scope:

- `boot_info::UsableRegions`: pure, host-testable region arithmetic.
- `kernel/src/frame.rs`: bump + intrusive free-stack frame allocator.
- `kernel/src/heap.rs`: address-ordered free-list heap with splitting and
  bidirectional coalescing, plus the `#[global_allocator]` shim.
- A boot-time selftest proving both layers, which exits with the failure
  code if any invariant does not hold.
- Host unit tests for the region arithmetic.
- Documentation updates (README roadmap/status, design.md open question, two
  stale milestone-number comments).

Explicitly out of scope:

- **Page tables and the stack guard page.** `README.md` currently promises
  "Milestone 6's stack guard page will make a genuine stack-overflow double
  fault happen naturally". That requires page-table manipulation, which
  `docs/design.md` places outside MVP scope ("No custom page tables in
  MVP"). This milestone does not add paging; the README sentence is
  corrected to point at future work instead.
- Buddy allocation, slab caches, per-CPU caches. Single-core kernel, no
  contention to optimize for.
- Freeing the heap's backing frames. The heap is created once at boot and
  lives for the life of the kernel.

## Prerequisites verified before design

These were checked against the tree at `f28fd57`, not assumed:

- **`alloc` needs no feature gates.** A throwaway probe crate built for
  `x86_64-unknown-none` on the pinned `nightly-2026-08-23` with
  `extern crate alloc`, `Vec`, `Box`, and a `#[global_allocator]` compiles
  clean, with no `#![feature(...)]` and no `#[alloc_error_handler]`. The
  default OOM handler panics, which routes into the kernel's existing
  `#[panic_handler]` and exits `Failed`. Heap exhaustion therefore reports
  itself on serial for free.
- **`.bss` is zeroed by the loader.** `bootloader/src/elf.rs` zeroes each
  segment's whole page allocation before copying `filesz` bytes, so a
  `static mut FRAMES: Option<FrameAllocator> = None` is genuinely `None` at
  entry.
- **`cargo test -p boot_info` already builds a host test harness**, despite
  the crate being `#![no_std]`. No `cfg_attr(not(test), no_std)` needed.
- **All memory the kernel still needs is `LOADER_DATA`, never
  `CONVENTIONAL`.** The kernel image (`elf.rs`), the kernel stack and the
  memory map (`handoff.rs`), `BootInfo` (`main.rs`), and the region array
  (`memory.rs`) all allocate as `LOADER_DATA`. Filtering on the existing
  `MemoryRegion::is_usable()` therefore excludes every live kernel
  allocation automatically — no hand-maintained exclusion list.
- **`interrupts::without_interrupts()` already exists** and was written for
  this milestone; its comment says so. It saves and restores `IF` rather
  than blindly re-enabling, and deliberately omits `options(nomem)` so it
  acts as a compiler barrier. The allocators use it as-is.

## Architecture

### `boot_info::UsableRegions` — the testable half

An iterator over `&[MemoryRegion]` yielding page-aligned `(start, frames)`
runs. Pure arithmetic: no raw pointers, no statics, no `unsafe`. It lives in
`boot_info` because it operates entirely on `MemoryRegion`, which is defined
there, and because that crate already host-tests.

Rules, each with a host test:

- Skip any region where `!is_usable()` (kind is not `CONVENTIONAL`).
- Round `start` up and `end` down to 4 KiB boundaries.
- Drop runs that round away to zero frames.
- Skip the frame at physical address 0, so an allocated frame address is
  never null and `NonNull` is always satisfiable.

### `kernel/src/frame.rs` — the frame allocator

```rust
static mut FRAMES: Option<FrameAllocator> = None;

struct FrameAllocator {
    runs: UsableRegions,
    cursor: u64,
    run_end: u64,
    free: Option<NonNull<FreeFrame>>,
    total: u64,
    in_use: u64,
}
```

- `alloc_frame() -> Option<u64>`: pop the intrusive free stack if non-empty,
  else advance the bump cursor, moving to the next run when the current one
  is exhausted; `None` when both sources are dry.
- `dealloc_frame(addr)`: write a `FreeFrame { next }` node into the frame's
  own first 8 bytes and push it. Zero side metadata. Sound because the
  kernel runs on UEFI's identity mapping, the premise `kernel.ld` already
  records.
- `alloc_contiguous(n) -> Option<u64>`: bump path only — a LIFO free stack
  cannot promise adjacency. Serves the heap's single 256-frame request.
  Skips to the next run when the current one cannot fit `n`.

Global access goes through `&raw mut FRAMES`, matching the `static mut`
convention already used in `console.rs` and `idt.rs` (required under edition
2024), each call wrapped in `interrupts::without_interrupts`.

### `kernel/src/heap.rs` — the heap

```rust
struct FreeBlock { size: usize, next: Option<NonNull<FreeBlock>> }  // 16 bytes
```

An address-ordered singly linked list of free blocks, each block's header
stored inside the block itself.

- `init(start, size)`: one block spanning the whole 1 MiB.
- `alloc(layout)`: first fit. Within a candidate block, compute the aligned
  start for `layout.align()`. Front padding becomes its own free block when
  it is at least 16 bytes, and is otherwise absorbed into the allocation.
  The tail splits off on the same threshold. Every request is rounded up to
  at least `size_of::<FreeBlock>()` and to `align_of::<FreeBlock>()`, so any
  block can host a header once freed.
- `dealloc(ptr, layout)`: insert in address order, then merge with the
  previous and next neighbours when physically adjacent. Both directions —
  this is what keeps repeated `Vec` doubling from fragmenting the heap into
  unusable slivers.
- `free_bytes()`: walk and sum. Exists so the selftest can assert the heap
  returns to its exact starting value, which only happens if coalescing is
  correct.

The `#[global_allocator]` is a unit struct whose `alloc`/`dealloc` wrap the
list walk in `interrupts::without_interrupts`. Its `unsafe impl Sync` safety
comment cites that critical section — an accurate claim about code, not a
promise about future discipline.

### Heap sizing and placement

256 contiguous frames (1 MiB), requested once at `heap::init`. QEMU hands
the kernel hundreds of MiB of conventional memory, so this is comfortably
available, and 1 MiB is ample for the selftest plus Milestone 7 keyboard
buffering.

## Integration with the existing kernel

Init order in `kernel_main`, after `console::init` and before
`interrupts::enable()`:

```
frame::init(regions)   ->  "frame allocator: N usable frames (M MiB)"
heap::init()           ->  "heap: 1 MiB @ 0x... (256 frames)"
selftest_alloc()       ->  "alloc: Box/Vec/String OK, heap balanced"
```

Two ordering constraints, both load-bearing:

- **After `console::init`.** `xtask`'s `check_screen_has_text` pixel-matches
  the top-left 8x16 cell against `KERNEL_FIRST_GLYPH`, which is the letter
  `f` from "framebuffer painted" — the first line written after the console
  exists. Initialising the allocators earlier would change the first glyph
  on screen and fail `cargo xtask run` with a message about glyphs that says
  nothing about allocators.
- **Before `interrupts::enable()`.** Bring-up then runs single-threaded with
  no reentrancy to reason about, matching how `gdt`/`idt`/`pic`/`pit` are
  already sequenced.

## Error handling

Every failure reports on serial before exiting, consistent with the rest of
the kernel:

| Failure | Handling |
|---|---|
| No usable regions in the memory map | serial line, `qemu_exit(Failed)` |
| Fewer than 256 contiguous frames for the heap | serial line, `qemu_exit(Failed)` |
| Any selftest invariant violated | serial line naming the invariant, `qemu_exit(Failed)` |
| Heap exhausted at runtime | default OOM handler panics into the existing `#[panic_handler]`, which prints and exits `Failed`. No new code. |

## Testing

`xtask` asserts on the **exit code** and the **screen capture**, never on
serial trace content — the trace is only streamed to stdout. So the kernel
is the assertion engine, following the pattern the keyboard check already
established ("no input within 10s" then `Failed`). **No `xtask` changes are
required by this milestone.**

1. **Host tests, `cargo test -p boot_info`** — `UsableRegions`:
   non-conventional regions excluded; unaligned start and end clamped
   inward; sub-page runs dropped; the frame at physical 0 skipped; an empty
   map yielding nothing.
2. **In-kernel selftest**, before `sti`:
   - frame `alloc -> dealloc -> alloc` returns the same address (proves the
     intrusive free stack)
   - `Vec<u64>` pushed past several reallocations holds the right values
   - `Box` and `String` round-trip
   - `free_bytes()` returns to its exact pre-test value (proves
     bidirectional coalescing)
3. **Existing suite must stay green, unchanged**: `cargo test -p xtask`
   (3 tests), `cargo xtask run` (exit 33 plus the glyph assertion),
   `cargo xtask test` (exit 35).

## Documentation changes

- `README.md`: tick roadmap item 6; update the Status paragraph; rewrite the
  guard-page sentence to reference future work rather than this milestone.
- `docs/design.md`: resolve the "hand-rolled vs `linked_list_allocator`"
  open question in favour of hand-rolled, with the reasoning.
- `boot_info/src/lib.rs`: fix the stale "Milestone 5's frame allocator"
  comment on `MemoryRegion::kind` (renumbering left it pointing at the wrong
  milestone).
- `kernel/src/interrupts.rs`: same stale-renumber fix in the
  `without_interrupts` comment, which also says "Milestone 5's frame
  allocator".
- `docs/plans/milestone-6-frame-allocator.md`: the implementation plan.
