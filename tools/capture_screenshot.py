#!/usr/bin/env python3
"""Capture the README screenshot: boot the kernel, type into it, screendump.

Run `cargo xtask run` first so `target/esp` and `target/OVMF_VARS.fd` exist,
then:

    python3 tools/capture_screenshot.py

Writes `docs/images/boot.png`.

Two things about this script are load-bearing and not obvious:

**It deliberately omits `-device isa-debug-exit`.** `xtask` always passes it,
which is how `qemu_exit::exit` shuts QEMU down with a verdict exit code.
Without that device the port write goes nowhere and `exit` falls through
into its own `hlt` loop -- with interrupts still enabled. IRQ1 keeps firing,
so every subsequent keystroke keeps printing to the framebuffer. That is
what makes an interactive screenshot possible at all, and it is the idle
echo loop `docs/design.md`'s boot flow describes.

**It copies OVMF_VARS.fd first.** The pflash VARS drive is opened
read-write. Sharing one file between two concurrent QEMU instances (say,
this script and a manual `qemu-system-x86_64` in another terminal) corrupts
the firmware's boot entries: the guest never reaches the bootloader, and you
get a 640x480 firmware-mode capture with an empty serial log instead.

Like `generate_font.py` and `ppm_to_png.py`, this is a one-off authoring
tool, not part of the build. Its output is committed; nobody cloning the
repo needs to run it.
"""

import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OVMF_CODE = "/usr/share/OVMF/OVMF_CODE_4M.fd"

SOCK = "/tmp/rust_bl_shot_monitor.sock"
PPM = "/tmp/rust_bl_shot.ppm"
SERIAL = "/tmp/rust_bl_shot_serial.log"
STDERR = "/tmp/rust_bl_shot_stderr.log"
VARS = "/tmp/rust_bl_shot_VARS.fd"

OUTPUT = REPO / "docs/images/boot.png"
TEXT = "hello world"
READY = "waiting for a keypress"


def serial_text():
    try:
        return Path(SERIAL).read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return ""


def main():
    os.chdir(REPO)

    staged = REPO / "target/esp/EFI/BOOT/BOOTX64.EFI"
    if not staged.exists():
        raise SystemExit(f"{staged} is missing -- run `cargo xtask run` first")

    for stale in (SOCK, PPM, SERIAL):
        Path(stale).unlink(missing_ok=True)
    shutil.copyfile(REPO / "target/OVMF_VARS.fd", VARS)
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)

    # stderr goes to a file, never an unread pipe: a full pipe buffer blocks
    # the emulator itself.
    with open(STDERR, "wb") as errlog:
        qemu = subprocess.Popen(
            [
                "qemu-system-x86_64",
                "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
                "-drive", f"if=pflash,format=raw,file={VARS}",
                "-drive", "format=raw,file=fat:rw:target/esp",
                "-m", "256M",
                "-display", "none",
                "-serial", f"file:{SERIAL}",
                "-monitor", f"unix:{SOCK},server,nowait",
                "-no-reboot",
            ],
            stdout=subprocess.DEVNULL,
            stderr=errlog,
        )
        try:
            for _ in range(150):
                if os.path.exists(SOCK):
                    break
                time.sleep(0.1)
            else:
                raise SystemExit("QEMU monitor socket never appeared")

            # Wait for the kernel to announce it is idling, rather than
            # guessing with a fixed sleep.
            for _ in range(400):
                if READY in serial_text():
                    break
                if qemu.poll() is not None:
                    raise SystemExit(f"QEMU exited early ({qemu.returncode})")
                time.sleep(0.1)
            else:
                raise SystemExit(
                    f"kernel never reached {READY!r}; serial tail:\n"
                    f"{serial_text()[-500:]}"
                )

            mon = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            mon.connect(SOCK)
            mon.settimeout(2)

            def send(command):
                mon.sendall((command + "\n").encode())
                time.sleep(0.3)
                try:
                    mon.recv(65536)  # drain the monitor's prompt
                except socket.timeout:
                    pass

            # QEMU sendkey names: letters are themselves, space is "spc".
            for char in TEXT:
                send(f"sendkey {'spc' if char == ' ' else char}")

            time.sleep(1)
            send(f"screendump {PPM}")

            for _ in range(60):
                if os.path.exists(PPM) and os.path.getsize(PPM) > 0:
                    break
                time.sleep(0.2)
            else:
                raise SystemExit("screendump produced no file")
            time.sleep(0.5)
        finally:
            qemu.terminate()
            try:
                qemu.wait(timeout=5)
            except subprocess.TimeoutExpired:
                qemu.kill()

    subprocess.run(
        [sys.executable, str(REPO / "tools/ppm_to_png.py"), PPM, str(OUTPUT)],
        check=True,
    )


if __name__ == "__main__":
    main()
