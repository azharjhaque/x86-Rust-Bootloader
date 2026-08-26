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
