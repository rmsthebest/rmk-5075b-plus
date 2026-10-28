#!/usr/bin/env python3
"""Decodes dump/info.bin, the information blocks at 0x1FFFF000-0x1FFFF7FF
(RM 3.2.2): a hex dump per block with the manual's name for it, and the
parts the manual describes. The other fields aren't documented; the ROM
disassembly should show which words it reads.

The unique ID (0x1FFFF7E8) identifies this chip: shown only with --uid.

Usage: python3 -I rom/tools/info.py [--uid] [FILE]   (default $ROM_WORK/dump/info.bin)
"""

import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
# The dump, the Ghidra project and everything generated from the dump live
# outside the repo (rom/README.md): $ROM_WORK, by default ../wb32-rom-re
# beside the repo.
WORK = Path(os.environ.get("ROM_WORK") or HERE.parent.parent / "wb32-rom-re")
BASE = 0x1FFFF000
BLOCKS = ["0 (read-only)", "1 (read-only)", "2 (read-only)", "3 (read-only)", "4 boot control",
          "5 user code write protection", "6 secondary development protection",
          "7 reboot configuration, OTP from 0x1FFFF720"]
UID = 0x7E8


def main():
    args = sys.argv[1:]
    show_uid = "--uid" in args
    files = [a for a in args if a != "--uid"]
    data = Path(files[0] if files else WORK / "dump/info.bin").read_bytes()
    if len(data) != 0x800:
        sys.exit(f"expected 2048 bytes, got {len(data)}")
    for b, title in enumerate(BLOCKS):
        block = data[b * 0x100:(b + 1) * 0x100]
        fill = {0x00: "all 0x00", 0xFF: "all 0xFF"}.get(block[0]) if len(set(block)) == 1 else None
        print(f"block {title} @ {BASE + b * 0x100:#010x}: {fill or ''}")
        if fill:
            continue
        for off in range(0, 0x100, 16):
            row = block[off:off + 16]
            if len(set(row)) == 1 and row[0] in (0x00, 0xFF):
                continue
            a = b * 0x100 + off
            if not show_uid and a <= UID + 11 and UID < a + 16:
                row = bytes(0x55 if UID <= a + i < UID + 12 else c for i, c in enumerate(row))
            words = " ".join(int.from_bytes(row[i:i + 4], "little").to_bytes(4, "big").hex() for i in range(0, 16, 4))
            print(f"  {BASE + a:#010x}: {words}")
    uid = data[UID:UID + 12]
    print(f"unique ID @ {BASE + UID:#010x}: {uid.hex() if show_uid else '(hidden, 0x55 above; --uid shows it)'}")


if __name__ == "__main__":
    main()
