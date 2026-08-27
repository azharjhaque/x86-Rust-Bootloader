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

impl PixelFormatKind {
    /// Checked conversion from the raw `u32` stored in [`FrameBufferInfo`].
    ///
    /// `FrameBufferInfo` stores the pixel format as a plain `u32` rather
    /// than this enum precisely so that reading it can never itself be
    /// undefined behavior: forming a `&PixelFormatKind` reference over a
    /// value the enum has no variant for is UB the instant the reference
    /// exists, before any code even gets to check it. Going through
    /// `u32` and this fallible conversion keeps that check in ordinary,
    /// safe code.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Rgb),
            1 => Some(Self::Bgr),
            _ => None,
        }
    }
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
    /// Raw [`PixelFormatKind`] discriminant. Stored as `u32`, not the enum
    /// itself, so that a corrupted or mismatched-version value can never
    /// produce an invalid enum discriminant in memory — see
    /// [`PixelFormatKind::from_u32`] and [`Self::pixel_format`].
    pub pixel_format_raw: u32,
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
            pixel_format_raw: pixel_format as u32,
            _pad: 0,
        }
    }

    /// The pixel format, or `None` if `pixel_format_raw` is not a value
    /// [`PixelFormatKind`] defines. [`BootInfo::is_valid`] checks this is
    /// `Some` before the struct is trusted at all.
    pub const fn pixel_format(&self) -> Option<PixelFormatKind> {
        PixelFormatKind::from_u32(self.pixel_format_raw)
    }
}

/// One entry of the UEFI memory map, flattened to a stable layout.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub start: u64,
    pub pages: u64,
    /// The raw UEFI `MemoryType` value. Milestone 6's frame allocator
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
/// testable on the host, where a mistake is a red test rather than a
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

            // This data crossed a binary boundary and is not trusted. A
            // descriptor whose span overflows is skipped rather than
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
    /// compatible version of this crate, and its framebuffer's pixel
    /// format is one this crate defines. The kernel must not read
    /// `framebuffer.pixel_format()` — or anything else in this struct —
    /// unless this returns `true`.
    pub const fn is_valid(&self) -> bool {
        self.magic == BOOT_INFO_MAGIC
            && self.version == BOOT_INFO_VERSION
            && self.framebuffer.pixel_format().is_some()
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

// This crate's entire job is a fixed, hand-agreed binary layout shared by
// two separately compiled binaries. An accidental field reorder, an added
// field, or a size change on either side of that boundary would not be a
// compile error by default — it would surface as a mysterious runtime
// fault (or worse, silently wrong pixels/addresses) well after the point
// where either binary could still report anything. These assertions turn
// that class of mistake into a build failure instead.
const _: () = assert!(size_of::<BootInfo>() == 88);
const _: () = assert!(size_of::<FrameBufferInfo>() == 40);
const _: () = assert!(size_of::<MemoryRegion>() == 24);

#[cfg(test)]
mod tests {
    use super::*;

    /// A UEFI type that is not CONVENTIONAL. 2 is LOADER_DATA, which is
    /// exactly what the bootloader marks the kernel image, stack, and
    /// `BootInfo` as — so this is the case that keeps the allocator from
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
