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
/// Call once, before any screen output should reach the console. Invalid
/// framebuffer descriptions leave it disabled.
pub unsafe fn init(framebuffer: &FrameBufferInfo) {
    // SAFETY: init is called once before any writer can run.
    unsafe { CONSOLE = Console::new(framebuffer) };
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
    /// Construct a console only when every later framebuffer access is safe.
    ///
    /// The renderer stores one u32 per pixel and always addresses pixels
    /// through stride, so this validates the complete span it can reach.
    fn new(framebuffer: &FrameBufferInfo) -> Option<Self> {
        let format = framebuffer.pixel_format()?;
        let width = framebuffer.width as usize;
        let height = framebuffer.height as usize;
        let stride = framebuffer.stride as usize;

        if framebuffer.addr == 0
            || framebuffer.addr % core::mem::align_of::<u32>() as u64 != 0
            || framebuffer.bytes_per_pixel != core::mem::size_of::<u32>() as u32
            || stride < width
            || width < GLYPH_WIDTH
            || height < GLYPH_HEIGHT
        {
            return None;
        }

        let pixels = stride.checked_mul(height)?;
        let bytes = pixels.checked_mul(core::mem::size_of::<u32>())?;
        let bytes = u64::try_from(bytes).ok()?;
        if bytes > framebuffer.size || framebuffer.addr.checked_add(bytes).is_none() {
            return None;
        }

        Some(Self {
            framebuffer: *framebuffer,
            column: 0,
            row: 0,
            columns: width / GLYPH_WIDTH,
            rows: height / GLYPH_HEIGHT,
            foreground: pack(format, 0xE0, 0xE0, 0xE0),
            background: pack(format, 0x00, 0x33, 0x99),
        })
    }

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
                // SAFETY: Console::new established a nonzero full-cell
                // geometry, stride >= width, and a checked framebuffer byte
                // span. This cell's coordinates are inside that span.
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

        // SAFETY: Console::new proved height >= GLYPH_HEIGHT and the whole
        // stride * height pixel span is inside the framebuffer. copy is
        // correct because source and destination overlap.
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
