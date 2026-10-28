#!/usr/bin/env python3
"""Reads or sets akko-rmk's key debounce over Vial channel 0xD6 (src/main.rs).

  eager  a press counts at its first reading (the default, ~4 ms sooner)
  defer  a press waits out the 5 ms debounce too, as QMK's default: for a
         switch that chatters and types phantom presses
  (none) print the current setting

Releases always wait 5 ms. The keyboard saves the setting with its lighting
record, so it survives a replug.

Usage: python3 -I tools/debounce.py [eager|defer]
"""

import os
import select
import sys
import time
from pathlib import Path

CHANNEL = 0xD6
ARG = {"eager": 1, "defer": 2}


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


def main():
    if len(sys.argv) > 2 or (len(sys.argv) == 2 and sys.argv[1] not in ARG):
        sys.exit(__doc__.strip())
    arg = ARG.get(sys.argv[1], 0) if len(sys.argv) == 2 else 0
    path = rmk_raw_hid()
    if not path:
        sys.exit("akko-rmk isn't connected over USB (cable in, USB mode)")
    fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
    try:
        os.write(fd, bytes([0x00, 0x08, CHANNEL, arg]) + bytes(29))
        deadline = time.monotonic() + 0.5
        while True:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([fd], [], [], left)[0]:
                sys.exit("no answer (an image without channel 0xD6?)")
            data = os.read(fd, 64)
            if data[:2] == bytes([0x08, CHANNEL]):
                break
    finally:
        os.close(fd)
    print("presses wait out the debounce (defer)" if data[3] else "presses count at once (eager)")


if __name__ == "__main__":
    main()
