#!/usr/bin/env python3
"""Convert a binary PPM (P6) to PNG.

QEMU's `screendump` monitor command writes PPM, which neither GitHub nor a
browser will render. This converts one to a PNG for the README.

Like `generate_font.py`, this is a one-off authoring tool, not part of the
build: its output (`docs/images/boot.png`) is committed, and nobody cloning
the repo needs to run it. That is why it may use Python and zlib freely
while `xtask` stays dependency-free -- the constraint applies to the build
and test pipeline that contributors actually run, not to a step that
produces a static asset.

Usage:
    python3 tools/ppm_to_png.py <input.ppm> <output.png>
"""

import struct
import sys
import zlib


def read_ppm(path):
    """Parse a binary PPM, returning (width, height, rgb_bytes)."""
    with open(path, "rb") as handle:
        data = handle.read()

    # The header is whitespace-separated tokens, with '#' comments legal
    # between them. Scan token by token rather than assuming the exact
    # "P6\n<w> <h>\n<maxval>\n" layout QEMU happens to emit.
    tokens = []
    offset = 0
    while len(tokens) < 4:
        while offset < len(data) and data[offset : offset + 1].isspace():
            offset += 1
        if offset < len(data) and data[offset : offset + 1] == b"#":
            while offset < len(data) and data[offset : offset + 1] != b"\n":
                offset += 1
            continue
        start = offset
        while offset < len(data) and not data[offset : offset + 1].isspace():
            offset += 1
        tokens.append(data[start:offset])
    # Exactly one whitespace byte separates the final header token from the
    # pixel payload; anything more would be part of the payload itself.
    offset += 1

    magic, width, height, maxval = tokens
    if magic != b"P6":
        raise SystemExit(f"{path}: not a binary PPM (magic {magic!r}, expected b'P6')")
    if maxval != b"255":
        raise SystemExit(f"{path}: maxval is {maxval!r}, only 255 is supported")

    width, height = int(width), int(height)
    expected = width * height * 3
    pixels = data[offset:]
    if len(pixels) != expected:
        raise SystemExit(
            f"{path}: pixel payload is {len(pixels)} bytes, expected {expected} "
            f"for {width}x{height} RGB"
        )
    return width, height, pixels


def chunk(tag, payload):
    """One PNG chunk: length, type, payload, CRC32 of type+payload."""
    body = tag + payload
    return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))


def write_png(path, width, height, pixels):
    # Each scanline is prefixed with its filter type. 0 (None) keeps this
    # simple; the screen is mostly a flat background, which zlib's LZ77
    # stage collapses regardless.
    raw = bytearray()
    stride = width * 3
    for y in range(height):
        raw.append(0)
        raw += pixels[y * stride : (y + 1) * stride]

    ihdr = struct.pack(
        ">IIBBBBB",
        width,
        height,
        8,  # bit depth
        2,  # colour type 2 = truecolour RGB
        0,  # deflate
        0,  # adaptive filtering
        0,  # no interlace
    )

    with open(path, "wb") as handle:
        handle.write(b"\x89PNG\r\n\x1a\n")
        handle.write(chunk(b"IHDR", ihdr))
        handle.write(chunk(b"IDAT", zlib.compress(bytes(raw), 9)))
        handle.write(chunk(b"IEND", b""))


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    source, destination = sys.argv[1], sys.argv[2]
    width, height, pixels = read_ppm(source)
    write_png(destination, width, height, pixels)
    print(f"{source} -> {destination} ({width}x{height})")


if __name__ == "__main__":
    main()
