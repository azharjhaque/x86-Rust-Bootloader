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
fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset + 8)?;
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
        let base = phoff as usize + index * phentsize as usize;

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

        load_segment(image, &segment)?;
        loaded_any = true;

        let seg_start = align_down(segment.vaddr);
        let seg_end = segment
            .vaddr
            .checked_add(segment.memsz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
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
    let total = padding + segment.memsz;
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
