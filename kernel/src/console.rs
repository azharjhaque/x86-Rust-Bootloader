//! A text console over the linear framebuffer.
//!
//! The kernel's other output channel, COM1, does not exist on most real
//! hardware — so without this, everything the kernel says is invisible
//! outside QEMU. There is no allocator, so the console owns no buffer: it
//! draws straight into the framebuffer and scrolls by moving pixels.

use boot_info::{FrameBufferInfo, PixelFormatKind};

use crate::font::{self, GLYPH_HEIGHT, GLYPH_WIDTH};

/// Everything needed to paint text, captured once at init.
struct Console {
    framebuffer: FrameBufferInfo,
    /// Cursor position in character cells, not pixels.
    column: usize,
    row: usize,
    columns: usize,
    rows: usize,
    foreground: u32,
    background: u32,
}

/// `static mut` rather than a lock because this kernel is single-core and
/// every writer goes through `_print`, which already runs inside
/// `without_interrupts`.
static mut CONSOLE: Option<Console> = None;

/// Pack an RGB triple into the framebuffer's pixel layout.
///
/// The two formats differ only in byte order, and the bootloader rejects
/// anything else, so this cannot fail.
fn pack(format: PixelFormatKind, red: u8, green: u8, blue: u8) -> u32 {
    let (r, g, b) = (red as u32, green as u32, blue as u32);
    match format {
        PixelFormatKind::Rgb => r | (g << 8) | (b << 16),
        PixelFormatKind::Bgr => b | (g << 8) | (r << 16),
    }
}

/// Prepare the console. Until this runs, [`write_byte`] does nothing.
///
/// # Safety
/// Call once, before any `kprintln!` that should reach the screen, with a
/// `FrameBufferInfo` the bootloader validated.
pub unsafe fn init(framebuffer: &FrameBufferInfo) {
    let format = framebuffer.pixel_format().unwrap_or(PixelFormatKind::Bgr);

    let console = Console {
        framebuffer: *framebuffer,
        column: 0,
        row: 0,
        columns: framebuffer.width as usize / GLYPH_WIDTH,
        rows: framebuffer.height as usize / GLYPH_HEIGHT,
        foreground: pack(format, 0xE0, 0xE0, 0xE0),
        background: pack(format, 0x00, 0x33, 0x99),
    };

    unsafe { CONSOLE = Some(console) };
}

/// Write one byte to the screen, or do nothing if [`init`] has not run.
pub fn write_byte(byte: u8) {
    // SAFETY: single-core, and every caller reaches here through `_print`,
    // which holds interrupts off for the duration.
    let console = match unsafe { (&raw mut CONSOLE).as_mut().and_then(|c| c.as_mut()) } {
        Some(console) => console,
        None => return,
    };
    console.write_byte(byte);
}

impl Console {
    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            // Serial wants CR; the screen has no use for it.
            b'\r' => {}
            byte => {
                if self.column >= self.columns {
                    self.newline();
                }
                self.draw_glyph(byte);
                self.column += 1;
            }
        }
    }

    fn newline(&mut self) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    /// Address of the pixel at (x, y).
    ///
    /// Always steps rows by `stride`, never by `width`: the framebuffer may
    /// have padding at the end of each scanline, and using `width` would
    /// shear the image progressively down the screen.
    fn pixel(&self, x: usize, y: usize) -> *mut u32 {
        let offset = y * self.framebuffer.stride as usize + x;
        (self.framebuffer.addr as *mut u32).wrapping_add(offset)
    }

    fn draw_glyph(&mut self, byte: u8) {
        let bitmap = font::glyph(byte);
        let origin_x = self.column * GLYPH_WIDTH;
        let origin_y = self.row * GLYPH_HEIGHT;

        for (dy, scanline) in bitmap.iter().enumerate() {
            for dx in 0..GLYPH_WIDTH {
                // Most significant bit is the leftmost pixel.
                let lit = scanline & (0x80 >> dx) != 0;
                let colour = if lit {
                    self.foreground
                } else {
                    self.background
                };
                // SAFETY: x < width and y < height by construction, and the
                // framebuffer spans width*height pixels at `stride` apart.
                unsafe {
                    self.pixel(origin_x + dx, origin_y + dy)
                        .write_volatile(colour)
                };
            }
        }
    }

    /// Move everything up one text row and clear the bottom one.
    ///
    /// This reads and writes the framebuffer directly rather than keeping a
    /// shadow buffer, which costs a few megabytes of copying per scroll but
    /// needs no allocator. A boot trace shorter than the screen never
    /// scrolls at all.
    fn scroll(&mut self) {
        let stride = self.framebuffer.stride as usize;
        let height = self.framebuffer.height as usize;
        let shift = GLYPH_HEIGHT * stride;

        let base = self.framebuffer.addr as *mut u32;
        let pixels = height * stride;

        // SAFETY: source and destination are both inside the framebuffer,
        // and `copy` (not `copy_nonoverlapping`) is correct because they
        // overlap.
        unsafe {
            core::ptr::copy(base.add(shift), base, pixels - shift);
        }

        // Clear the freed row.
        for y in (height - GLYPH_HEIGHT)..height {
            for x in 0..self.framebuffer.width as usize {
                unsafe { self.pixel(x, y).write_volatile(self.background) };
            }
        }
    }
}
