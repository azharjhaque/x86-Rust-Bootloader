# Milestone 5: Framebuffer Text Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the kernel a screen it can actually write to. Embed a bitmap
font, draw glyphs into the GOP framebuffer, build a console with a cursor
that wraps and scrolls, and fan `kprintln!` out to both serial and screen —
so the boot trace that currently exists only on a wire also appears on the
monitor, including on real hardware where there is no serial port at all.

**Architecture:** Three new kernel modules. `font` holds the glyph bitmaps
and a lookup. `console` owns a cursor and turns bytes into glyph blits,
handling newline, wrap and scroll. `serial::_print` grows a second
destination. `xtask` gains a screen capture over the QEMU monitor so "text
reached the screen" is a checked assertion rather than something a human
squints at.

**Tech Stack:** Rust nightly (pinned `nightly-2026-08-23`),
`x86_64-unknown-none`, no crates.io dependencies. QEMU + OVMF, plus QEMU's
monitor `screendump` command.

**Spec:** [docs/design.md](../design.md)

## Global Constraints

- Rust **nightly**, pinned to `nightly-2026-08-23`.
- **No crates.io dependencies in `kernel`** — `kernel` may depend only on the
  local `boot_info`. `xtask` stays `std`-only.
- QEMU + OVMF is the only tested target.
- All assembly is inline `core::arch::asm!`.
- Edition 2024; Rust 2024 requires `#[unsafe(no_mangle)]`.
- Exit-code contract unchanged: `Success = 0x10` → 33, `Failed = 0x11` → 35.
- **No page tables and no allocator in this milestone.** The console is a
  fixed-size structure in `.bss`; nothing here may need `alloc`.
- Interrupts and the existing GDT/IDT/PIC/PIT/keyboard work stay exactly as
  they are.

## Design decisions for this milestone

**This milestone was reordered ahead of the allocator.** The design spec
lists memory management as Milestone 5 and text rendering as part of the
polish milestone. Swapping them is deliberate: right now the kernel's entire
visible output on real hardware is one solid blue rectangle, because
everything it says goes to COM1 and most machines have no serial port. The
allocator would have added nothing visible and would have been built without
that feedback. Text rendering is independent of the allocator — a fixed-size
console needs no heap — so nothing is blocked by the swap. The frame
allocator and heap become Milestone 6, and polish becomes Milestone 7.

**The font is extracted from a system console font, not hand-authored.**
Hand-typing 95 glyph bitmaps is data entry, not learning, and a single wrong
bit renders as silent garbage. Debian's `console-setup` package states
plainly that "All console fonts are public domain by nature", so embedding
one raises no licensing question against this repo's MIT licence. A
committed generator script records the provenance, and the generated
`font.rs` is committed too so the build needs no Python.

The specific font is `Lat15-VGA16.psf.gz` — the classic IBM PC VGA 8×16
face. Verified before this plan was written by parsing it and rendering
glyphs as ASCII art: `A`, `B`, `a`, `g` and `!` all came out correct.

**The screen test is a pixel assertion, not a human looking.** QEMU's
monitor accepts `screendump <path>` even under `-display none`, writing a
PPM of the guest framebuffer — verified working before this plan, including
that it captures the kernel's own 1280×800 surface rather than the firmware
console. That gives a real check: today the captured image contains
**exactly one distinct colour**, RGB (0, 51, 153), because the only thing
ever drawn is the blue fill. After this milestone it must contain more, with
foreground pixels where text belongs. Task 3 verifies the check fails on the
pre-text build before wiring the fan-out, so the assertion is known to be
capable of failing.

---

### Task 1: Embed the font

**Files:**
- Create: `tools/generate_font.py`
- Create: `kernel/src/font.rs` (generated, committed)
- Modify: `kernel/src/main.rs` (declare the module)

**Interfaces:**
- Produces: `font::GLYPH_WIDTH`, `font::GLYPH_HEIGHT`, and
  `font::glyph(byte: u8) -> &'static [u8; font::GLYPH_HEIGHT]`, consumed by
  Task 2's blitter.

- [ ] **Step 1: Write the generator**

`tools/generate_font.py`:

```python
#!/usr/bin/env python3
"""Generate kernel/src/font.rs from a PSF console font.

Run once; the output is committed so the build needs no Python. This script
exists to record where the font data came from, not as a build step.

Source: /usr/share/consolefonts/Lat15-VGA16.psf.gz (Debian console-setup),
the classic IBM PC VGA 8x16 face. Debian's copyright file for that package
states "All console fonts are public domain by nature", so embedding the
bitmaps raises no licensing question.

Only printable ASCII (0x20..=0x7E) is emitted — 95 glyphs. Anything outside
that range renders as '?' at runtime.
"""
import gzip
import sys

SRC = "/usr/share/consolefonts/Lat15-VGA16.psf.gz"
FIRST, LAST = 0x20, 0x7E

data = gzip.open(SRC, "rb").read()
if data[:2] != b"\x36\x04":
    sys.exit(f"{SRC}: not a PSF1 font")

charsize = data[3]
if charsize != 16:
    sys.exit(f"expected an 8x16 font, got charsize={charsize}")
glyphs = data[4:]

rows = []
for code in range(FIRST, LAST + 1):
    off = code * charsize
    body = ", ".join(f"0x{b:02x}" for b in glyphs[off : off + charsize])
    ch = chr(code)
    label = "space" if ch == " " else ch
    rows.append(f"    // {code:#04x} {label}\n    [{body}],")

print(f"generated {len(rows)} glyphs", file=sys.stderr)

with open("kernel/src/font.rs", "w") as f:
    f.write(f'''//! An 8x16 bitmap font.
//!
//! GENERATED by `tools/generate_font.py` from
//! `{SRC}` — do not edit by hand.
//!
//! Each glyph is {charsize} bytes, one per scanline, most significant bit
//! leftmost. Only printable ASCII ({FIRST:#04x}..={LAST:#04x}) is stored;
//! [`glyph`] substitutes `?` for anything else.

/// Width of a glyph in pixels. One byte per scanline, so this is 8.
pub const GLYPH_WIDTH: usize = 8;
/// Height of a glyph in pixels.
pub const GLYPH_HEIGHT: usize = {charsize};

const FIRST: u8 = {FIRST:#04x};
const LAST: u8 = {LAST:#04x};

static GLYPHS: [[u8; GLYPH_HEIGHT]; {len(rows)}] = [
{chr(10).join(rows)}
];

/// The bitmap for `byte`, or the bitmap for `?` if it is not printable.
pub fn glyph(byte: u8) -> &'static [u8; GLYPH_HEIGHT] {{
    let index = if (FIRST..=LAST).contains(&byte) {{
        (byte - FIRST) as usize
    }} else {{
        (b'?' - FIRST) as usize
    }};
    &GLYPHS[index]
}}
''')
```

- [ ] **Step 2: Run it**

From the workspace root:

```bash
python3 tools/generate_font.py
```

Expected: `generated 95 glyphs` on stderr, and `kernel/src/font.rs` created.

- [ ] **Step 3: Sanity-check the generated data**

The whole risk in generated data is that it is subtly wrong and renders as
garbage. Check one glyph by eye before trusting the rest:

```bash
python3 - <<'PY'
import re
src = open("kernel/src/font.rs").read()
# Pull the row that follows the comment for 'A' (0x41).
m = re.search(r"// 0x41 A\n\s*\[([^\]]*)\]", src)
row = [int(x, 16) for x in m.group(1).split(",")]
for b in row:
    print("".join("#" if b & (0x80 >> i) else "." for i in range(8)))
PY
```

Expected: a recognisable capital A — a point at the top, widening, with a
crossbar. If it is noise, the extraction is wrong; stop and report rather
than continuing.

- [ ] **Step 4: Declare the module and build**

Add `mod font;` to `kernel/src/main.rs` alongside the others.

```bash
cargo build -p kernel --target x86_64-unknown-none
```

Expected: builds. `font::glyph` has no caller yet, so add
`#[expect(dead_code)]` to it — `expect` rather than `allow` so the attribute
forces its own removal when Task 2 supplies the caller. `GLYPH_WIDTH` and
`GLYPH_HEIGHT` are `pub` consts and will not warn.

- [ ] **Step 5: Confirm the build is warning-free**

```bash
cargo xtask run
```

Expected: unchanged behaviour, still PASS. Nothing draws yet.

- [ ] **Step 6: Commit**

```bash
git add tools/generate_font.py kernel/src/font.rs kernel/src/main.rs
git commit -m "Embed a public-domain 8x16 VGA font"
```

---

### Task 2: The blitter and the console

**Files:**
- Create: `kernel/src/console.rs`
- Modify: `kernel/src/main.rs`

**Interfaces:**
- Consumes: `font::glyph`, and `boot_info::FrameBufferInfo`.
- Produces: `console::init(&FrameBufferInfo)` and
  `console::write_byte(u8)`, consumed by Task 3's fan-out.

- [ ] **Step 1: Write the console**

`kernel/src/console.rs`:

```rust
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
    let format = framebuffer
        .pixel_format()
        .unwrap_or(PixelFormatKind::Bgr);

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
                let colour = if lit { self.foreground } else { self.background };
                // SAFETY: x < width and y < height by construction, and the
                // framebuffer spans width*height pixels at `stride` apart.
                unsafe { self.pixel(origin_x + dx, origin_y + dy).write_volatile(colour) };
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
```

- [ ] **Step 2: Initialise it**

In `kernel/src/main.rs`, add `mod console;`. In `kernel_main`, after
`fill_screen` (so the fill does not erase the text) and before the existing
`kprintln!("framebuffer painted")`:

```rust
    // SAFETY: called once, and `info` was validated in `_start`.
    unsafe { console::init(&info.framebuffer) };
```

- [ ] **Step 3: Draw something and look at it**

Immediately after that, add a temporary probe so there is something to see
before the fan-out exists:

```rust
    for byte in b"console online" {
        console::write_byte(*byte);
    }
    console::write_byte(b'\n');
```

- [ ] **Step 4: Capture the screen and confirm text appeared**

```bash
cargo xtask run > /dev/null 2>&1
```

then, in a script file (background jobs break in inline `wsl.exe` commands):

```bash
cat > ~/scratch/shot.sh <<'EOF'
#!/bin/bash
cd "$HOME/projects/Rust_BL"
OUT=/tmp/shot.ppm; SOCK=/tmp/shot.sock; rm -f "$OUT" "$SOCK"
qemu-system-x86_64 \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=target/OVMF_VARS.fd \
  -drive format=raw,file=fat:rw:target/esp \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  -no-reboot -m 256M -display none -serial file:/tmp/shot.log \
  -monitor "unix:$SOCK,server,nowait" > /dev/null 2>&1 &
PID=$!
for _ in $(seq 1 60); do [ -S "$SOCK" ] && break; sleep 0.1; done
sleep 4.5
printf 'screendump %s\n' "$OUT" | nc -U "$SOCK" -w 2 > /dev/null 2>&1
sleep 0.5
wait $PID
python3 - "$OUT" <<'PY'
import sys
f = open(sys.argv[1],'rb'); f.readline()
w,h = map(int, f.readline().split()); f.readline()
px = f.read()
seen = set(px[i:i+3] for i in range(0, w*h*3, 3))
print(f"{w}x{h}, distinct colours: {len(seen)}")
print("TEXT PRESENT" if len(seen) > 1 else "NO TEXT — only the fill colour")
PY
EOF
bash ~/scratch/shot.sh
```

Expected: `1280x800, distinct colours: 2` and `TEXT PRESENT`. Before this
task the same check reported `distinct colours: 1`, so a passing result here
means glyphs really were drawn.

If it reports one colour, the blitter is writing somewhere the display is
not reading — check `stride` versus `width` in `pixel()` first.

- [ ] **Step 5: Remove the temporary probe**

Delete the four lines added in Step 3. Task 3 replaces them with the real
fan-out. Re-run `cargo xtask run` and confirm it still PASSes.

- [ ] **Step 6: Commit**

```bash
git add kernel/src/console.rs kernel/src/main.rs
git commit -m "Draw glyphs into the framebuffer with a scrolling console"
```

---

### Task 3: Fan output to both, and assert it in `xtask`

**Files:**
- Modify: `kernel/src/serial.rs`
- Modify: `xtask/src/main.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `console::write_byte` from Task 2.
- Produces: a `kprintln!` that reaches both destinations, and a
  `cargo xtask run` that fails if the screen stays blank.

- [ ] **Step 1: Send every byte to both destinations**

In `kernel/src/serial.rs`, the `Write` implementation currently calls
`write_byte` (the UART one). Extend it so each byte also goes to the
console:

```rust
impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            // Terminals expect CRLF; the console ignores the CR.
            if byte == b'\n' {
                write_byte(b'\r');
                crate::console::write_byte(b'\r');
            }
            write_byte(byte);
            crate::console::write_byte(byte);
        }
        Ok(())
    }
}
```

`console::write_byte` is a no-op until `console::init` runs, so the early
boot lines still reach serial only — which is correct, since the framebuffer
description is not known until `BootInfo` is validated.

- [ ] **Step 2: Prove the screen check can fail, before making it pass**

This ordering matters. Add the check to `xtask` first and watch it fail
against the current build, so a later pass means something.

In `xtask/src/main.rs`, add beside the other constants:

```rust
/// Where the guest framebuffer capture is written.
const SCREENSHOT: &str = "/tmp/rust_bl_screen.ppm";
```

Add a function that reads the capture and counts distinct colours:

```rust
/// Check that the guest actually drew something other than the background.
///
/// Before this milestone the captured framebuffer contained exactly one
/// colour, because a solid fill was the only thing ever drawn. More than one
/// means glyphs reached the screen. This is deliberately a weak assertion
/// about *what* was drawn and a strong one about *whether* anything was —
/// the serial trace already covers content.
fn check_screen_has_text() -> Result<(), String> {
    let data = fs::read(SCREENSHOT)
        .map_err(|e| format!("no screen capture at {SCREENSHOT}: {e}"))?;

    // PPM: "P6\n<w> <h>\n<maxval>\n" then raw RGB triples.
    let mut parts = data.splitn(4, |b| *b == b'\n');
    let magic = parts.next().unwrap_or(b"");
    if magic != b"P6" {
        return Err("screen capture is not a P6 PPM".to_string());
    }
    parts.next();
    parts.next();
    let pixels = parts.next().unwrap_or(b"");

    let mut first: Option<[u8; 3]> = None;
    for chunk in pixels.chunks_exact(3) {
        let rgb = [chunk[0], chunk[1], chunk[2]];
        match first {
            None => first = Some(rgb),
            Some(seen) if seen != rgb => return Ok(()),
            _ => {}
        }
    }
    Err("the screen shows a single flat colour — no text was drawn".to_string())
}
```

Have the monitor thread capture the screen before injecting keys, so the
capture happens while the kernel is still running. In `inject_keystrokes`,
before the key loop:

```rust
        // Capture the screen first: once a key lands the kernel finishes
        // and exits, and there is nothing left to photograph.
        let _ = fs::remove_file(SCREENSHOT);
        let _ = stream.write_all(format!("screendump {SCREENSHOT}\n").as_bytes());
        thread::sleep(Duration::from_millis(500));
```

Then in `run()`, after the exit code check passes, add the screen assertion:

```rust
        Some(EXPECTED_EXIT_CODE) => {
            if let Err(msg) = check_screen_has_text() {
                eprintln!("FAIL: {msg}");
                return ExitCode::FAILURE;
            }
            println!("PASS: bootloader exited with expected code {EXPECTED_EXIT_CODE}, and the screen has text");
            ExitCode::SUCCESS
        }
```

**Now run it with Step 1 reverted** (comment out the two
`crate::console::write_byte` lines) and confirm:

```
FAIL: the screen shows a single flat colour — no text was drawn
```

That is the check proving itself. Restore Step 1 afterwards.

- [ ] **Step 3: Run it for real**

```bash
cargo xtask run
```

Expected: the full trace on serial as before, and:

```
PASS: bootloader exited with expected code 33, and the screen has text
```

- [ ] **Step 4: Look at it**

```bash
cd ~/projects/Rust_BL && qemu-system-x86_64 -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd -drive if=pflash,format=raw,file=target/OVMF_VARS.fd -drive format=raw,file=fat:rw:target/esp -m 256M -serial stdio
```

Expected: the kernel's boot trace in white text on the blue background, and
typing a key adds a `key: 'x'` line to the screen as well as the terminal.
Needs WSLg or an X server; if no window appears, skip — the automated check
is authoritative.

- [ ] **Step 5: Confirm nothing else regressed**

```bash
cargo xtask test
XTASK_NO_KEYS=1 cargo xtask run
```

Expected: exit 35 for both — the ELF rejection path and the keyboard
deadline both still work.

- [ ] **Step 6: Update the README**

Add milestone 5 to the roadmap as complete, renumber the allocator to 6 and
polish to 7, and update the Status block. Replace the blue-screen caveat in
the real-hardware notes: the kernel now prints its boot trace on screen, so a
machine with no serial port finally shows something useful. Capture the real
trace rather than hand-writing it.

Also update `docs/design.md`'s milestone list for the reordering, following
the strikethrough-plus-resolution style already used there for earlier
deviations.

- [ ] **Step 7: Commit**

```bash
git add kernel/src/serial.rs xtask/src/main.rs README.md docs/design.md
git commit -m "Mirror the boot trace to the screen and assert it in xtask"
```

---

## After this plan

Milestone 5 is complete when Task 3 passes. What it deliberately leaves:

- **No colour control, no cursor rendering, no ANSI handling.** Text is one
  foreground colour on the fill colour, and there is no visible cursor.
- **No shadow buffer.** Scrolling moves pixels in the framebuffer, which is
  a multi-megabyte copy. Fine for a boot trace; the thing to revisit if
  anything ever prints in a loop.
- **Only printable ASCII.** Anything else renders as `?`. The kernel's own
  output is ASCII, so this is invisible in practice — but note the trace
  contains an em dash in a few messages, which will show as `?` on screen
  while remaining correct on serial. Worth changing those messages to ASCII
  if it looks wrong.
- **The allocator moves to Milestone 6**, and still needs its two
  prerequisites resolved first: `docs/design.md` currently rules out page
  tables, which contradicts the guard-page goal, and the kernel does not know
  where its stack is (the bootloader passes only the top, in `rsp`), so a
  guard page needs a new `BootInfo` field and a `BOOT_INFO_VERSION` bump.
