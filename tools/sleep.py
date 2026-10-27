#!/usr/bin/env python3
"""Reads or sets akko-rmk's sleep timeout over Vial channel 0xD7
(src/power.rs): how long the keyboard waits, with no key or knob input, before
it sleeps on Bluetooth or 2.4G (it never sleeps on USB).

  never              never sleep when idle (it still sleeps when the
                     computer does, or the battery is flat)
  1 3 5 10 20 30     minutes
  default            2 minutes, the vendor's default
  (none)             print the current setting

The keyboard saves the setting with its lighting record, so it survives a
replug.

Usage: python3 -I tools/sleep.py [never|1|3|5|10|20|30|default]
"""

import os
import select
import sys
import time
from pathlib import Path

CHANNEL = 0xD7
# The firmware's index for each setting (lighting/src/record.rs SLEEP_*).
INDEX = {"never": 0, "1": 1, "3": 2, "5": 3, "10": 4, "20": 5, "30": 6, "default": 7}
NAMES = {0: "never", 1: "1 min", 2: "3 min", 3: "5 min", 4: "10 min", 5: "20 min", 6: "30 min",
         7: "2 min (default)"}


def rmk_raw_hid():
    """/dev path of akko-rmk's Vial raw HID node, or None (as tools/bench.py)."""
    for h in sorted(Path("/sys/class/hidraw").iterdir()):
        dev = h.resolve() / "device"
        if "0003:0000342D:0000E4F6" not in (dev / "uevent").read_text().upper():
            continue
        if (dev / "report_descriptor").read_bytes().startswith(b"\x06\x60\xff"):
            return f"/dev/{h.name}"
    return None


def main():
    if len(sys.argv) > 2 or (len(sys.argv) == 2 and sys.argv[1] not in INDEX):
        sys.exit(__doc__.strip())
    arg = INDEX[sys.argv[1]] + 1 if len(sys.argv) == 2 else 0
    path = rmk_raw_hid()
    if not path:
        sys.exit("akko-rmk isn't connected (over USB)")
    fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
    try:
        os.write(fd, bytes([0x00, 0x08, CHANNEL, arg]) + bytes(29))
        deadline = time.monotonic() + 0.5
        while True:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([fd], [], [], left)[0]:
                sys.exit("no answer (an image without channel 0xD7?)")
            data = os.read(fd, 64)
            if data[:2] == bytes([0x08, CHANNEL]):
                break
    finally:
        os.close(fd)
    print("sleep after", NAMES.get(data[3], f"? ({data[3]})"))


if __name__ == "__main__":
    main()
