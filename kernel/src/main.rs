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
