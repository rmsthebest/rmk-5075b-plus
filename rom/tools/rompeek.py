#!/usr/bin/env python3
"""Reads the WB32's ROM bootloader and information blocks out of akko-rmk
over Vial channel 0xDA (src/main.rs `peek`), for studying the bootloader.
It only reads: the firmware refuses anything outside these two ranges and
never writes.

  ROM bootloader      0x1FFFE000-0x1FFFEFFF  -> rom.bin  (4096 bytes)
  information blocks  0x1FFFF000-0x1FFFF7FF  -> info.bin (2048 bytes)

Everything is read twice and the two reads must agree. The files and their
sha256 go to OUT (default $ROM_WORK/dump, rom/README.md). They hold the
vendor's code and this chip's unique ID: keep them out of the repo.
Needs an akko-rmk image built with the `rom-peek` feature.

Usage: python3 -I rom/tools/rompeek.py [OUT]
"""

import hashlib
import os
import select
import sys
import time
from pathlib import Path

CHANNEL = 0xDA
CHUNK = 24
RANGES = [("rom.bin", 0x1FFFE000, 0x1000), ("info.bin", 0x1FFFF000, 0x800)]
# $ROM_WORK, by default ../wb32-rom-re beside the repo.
OUT = Path(os.environ.get("ROM_WORK") or Path(__file__).resolve().parents[3] / "wb32-rom-re") / "dump"


def rmk_raw_hid():
    """/dev path of akko-rmk's Vial raw HID node, or None (as tools/bench.py)."""
    for h in sorted(Path("/sys/class/hidraw").iterdir()):
        dev = h.resolve() / "device"
        uevent = (dev / "uevent").read_text()
        # The 2.4G dongle has the same ids (and a Vial node): only "(RMK)".
        if "0003:0000342D:0000E4F6" not in uevent.upper() or "(RMK)" not in uevent:
            continue
        if (dev / "report_descriptor").read_bytes().startswith(b"\x06\x60\xff"):
            return f"/dev/{h.name}"
    return None


def peek(fd, addr):
    """Sends 08 da <addr LE> and returns the bytes the keyboard read there."""
    want = addr.to_bytes(4, "little")
    os.write(fd, bytes([0x00, 0x08, CHANNEL]) + want + bytes(26))
    deadline = time.monotonic() + 0.5
    while True:
        left = deadline - time.monotonic()
        if left <= 0 or not select.select([fd], [], [], left)[0]:
            sys.exit(f"no answer at {addr:#010x} (an image without the rom-peek feature?)")
        data = os.read(fd, 64)
        if data[:2] == bytes([0x08, CHANNEL]) and data[2:6] == want:
            break
    if data[6] != 0:
        sys.exit(f"refused at {addr:#010x}")
    return data[8:8 + data[7]]


def read_range(fd, start, size):
    out = b""
    while len(out) < size:
        part = peek(fd, start + len(out))
        if not part:
            sys.exit(f"empty answer at {start + len(out):#010x}")
        out += part
    return out[:size]


def main():
    if len(sys.argv) > 2 or sys.argv[1:2] in (["-h"], ["--help"]):
        sys.exit(__doc__.strip())
    out = Path(sys.argv[1]) if len(sys.argv) == 2 else OUT
    path = rmk_raw_hid()
    if not path:
        sys.exit("akko-rmk isn't connected (over USB)")
    fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
    try:
        dumps = []
        for name, start, size in RANGES:
            first, second = read_range(fd, start, size), read_range(fd, start, size)
            if first != second:
                diff = next(i for i in range(size) if first[i] != second[i])
                sys.exit(f"{name}: the two reads differ at {start + diff:#010x}")
            dumps.append((name, start, first))
    finally:
        os.close(fd)
    out.mkdir(parents=True, exist_ok=True)
    sums = []
    for name, start, data in dumps:
        (out / name).write_bytes(data)
        sums.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
        print(f"{name}: {len(data)} bytes from {start:#010x}")
    (out / "SHA256SUMS").write_text("".join(sums))
    rom = dumps[0][2]
    sp, reset = (int.from_bytes(rom[i:i + 4], "little") for i in (0, 4))
    inside = 0x1FFFE000 <= (reset & ~1) < 0x1FFFF000
    print(f"ROM vectors: SP {sp:#010x}, reset {reset:#010x} ({'inside' if inside else 'OUTSIDE'} the ROM)")
    print(f"written to {out}")


if __name__ == "__main__":
    main()
