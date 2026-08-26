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
