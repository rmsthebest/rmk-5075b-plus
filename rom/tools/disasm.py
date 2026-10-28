#!/usr/bin/env python3
"""A plain objdump listing of the ROM (Thumb, at its real address), with
the vector table named and every PC-relative literal resolved: the word it
loads and, when that is an address we know (out/labels.csv), its name. The
Ghidra export is the main view; this listing is a second, independent view
to check it against.

Usage: python3 -I rom/tools/disasm.py [--rom FILE] [--base ADDR] [--size N] [--out FILE]
  defaults: $ROM_WORK/dump/rom.bin, 0x1FFFE000, the whole file (at most
  4096), $ROM_WORK/out/rom.lst
"""

import argparse
import bisect
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
# The dump, the Ghidra project and everything generated from the dump live
# outside the repo (rom/README.md): $ROM_WORK, by default ../wb32-rom-re
# beside the repo.
WORK = Path(os.environ.get("ROM_WORK") or HERE.parent.parent / "wb32-rom-re")


def load_csv(path):
    rows = []
    for line in path.read_text().splitlines():
        a, n = line.split(",")
        rows.append((int(a, 0), n))
    return rows


def namer(labels):
    """addr -> "NAME" or "NAME+0xN" for an address at or above a label in
    the same 1 KB block (peripherals are 1 KB apart), else None."""
    addrs = [a for a, _ in labels]
    names = dict(labels)

    def name(addr):
        if addr in names:
            return names[addr]
        i = bisect.bisect_right(addrs, addr) - 1
        if i >= 0 and addr - addrs[i] < 0x400 and addr >> 10 == addrs[i] >> 10:
            return f"{names[addrs[i]]}+{addr - addrs[i]:#x}"
        return None

    return name


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rom", type=Path, default=WORK / "dump/rom.bin")
    ap.add_argument("--base", type=lambda s: int(s, 0), default=0x1FFFE000)
    ap.add_argument("--size", type=lambda s: int(s, 0), default=4096)
    ap.add_argument("--out", type=Path, default=WORK / "out/rom.lst")
    args = ap.parse_args()
    if not (WORK / "out/labels.csv").exists():
        sys.exit("run rom/tools/gen_labels.py first")
    rom = args.rom.read_bytes()[:args.size]
    name = namer(load_csv(WORK / "out/labels.csv"))
    vectors = dict(load_csv(WORK / "out/vectors.csv"))

    def word(addr):
        off = addr - args.base
        return int.from_bytes(rom[off:off + 4], "little") if 0 <= off <= len(rom) - 4 else None

    # The vector table runs while its entries are 0 or Thumb addresses inside
    # the image (entry 0 is the stack pointer).
    # The reserved core slots may hold data (the ROM has 0xFFFF0004 in slot 7).
    nvec = 1
    while nvec < len(vectors) and (w := word(args.base + 4 * nvec)) is not None and (
            nvec in (7, 8, 9, 10, 13) or w == 0 or (w & 1 and args.base <= w < args.base + len(rom))):
        nvec += 1
    header = [f"; {args.rom} at {args.base:#010x}, {len(rom)} bytes; vector table {nvec} entries\n"]
    for i in range(nvec):
        w = word(args.base + 4 * i)
        header.append(f"; vector {i:2} {vectors.get(i, f'irq{i - 16}'):<20} {w:#010x}\n")

    with tempfile.NamedTemporaryFile(suffix=".bin") as f:
        f.write(rom)
        f.flush()
        lst = subprocess.run(["arm-none-eabi-objdump", "-D", "-b", "binary", "-marm", "-Mforce-thumb",
                              f"--adjust-vma={args.base:#x}", f.name],
                             check=True, capture_output=True, text=True).stdout

    out = []
    for line in lst.splitlines():
        m = re.search(r"@ \(0x([0-9a-f]+)\)", line)
        if m and (w := word(int(m.group(1), 16))) is not None:
            n = name(w)
            line += f"  ; ={w:#010x}" + (f" {n}" if n else "")
        elif (m := re.match(r"\s*([0-9a-f]+):", line)) and (a := int(m.group(1), 16)) < args.base + 4 * nvec:
            i = (a - args.base) // 4
            line += f"  ; vector {i} {vectors.get(i, '')}"
        out.append(line + "\n")
    args.out.parent.mkdir(exist_ok=True)
    args.out.write_text("".join(header + out))
    print(f"{args.out}: {len(out)} lines, {nvec} vectors")


if __name__ == "__main__":
    main()
