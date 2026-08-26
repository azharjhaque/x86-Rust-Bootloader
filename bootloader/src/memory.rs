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
