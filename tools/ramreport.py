#!/usr/bin/env python3
"""Where the RAM goes in an image: sections, the stack, the biggest statics
and the biggest stack frames.

flip-link puts the stack at the bottom of RAM, below .data and .bss, so the
stack gets whatever they leave of RAM. `.ram2` (memory.x RAM2) is separate.

Usage: ramreport.py [elf]   (default: the release akko-rmk)
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ELF = ROOT / "target/thumbv7m-none-eabi/release/akko-rmk"
RAM_START = 0x2000_0000
RAM2 = (0x2000_7400, 0x2000_9000)
TOP = 15


def run(*args):
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout


def sections(elf):
    out = {}
    for line in run("arm-none-eabi-size", "-A", str(elf)).splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[0].startswith("."):
            out[parts[0]] = (int(parts[1]), int(parts[2]))
    return out


def symbols(elf):
    """(size, addr, kind, name) of every sized symbol."""
    syms = []
    for line in run("arm-none-eabi-nm", "-S", "-C", str(elf)).splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4:
            syms.append((int(parts[1], 16), int(parts[0], 16), parts[2], parts[3]))
    return syms


FRAME = re.compile(r"\bsub(?:\.w|w)?\s+sp,\s*(?:sp,\s*)?#(\d+)")
FUNC = re.compile(r"^[0-9a-f]+ <(.*)>:$")


def frames(elf):
    """The largest `sub sp, #n` of each function."""
    best = {}
    func = None
    out = run("arm-none-eabi-objdump", "-d", "-C", "--no-show-raw-insn", str(elf))
    for line in out.splitlines():
        m = FUNC.match(line)
        if m:
            func = m.group(1)
            continue
        m = FRAME.search(line)
        if m and func:
            best[func] = max(best.get(func, 0), int(m.group(1)))
    return sorted(((n, f) for f, n in best.items()), reverse=True)


def short(name, width=90):
    return name if len(name) <= width else name[: width - 3] + "..."


def main():
    elf = Path(sys.argv[1]) if len(sys.argv) > 1 else ELF
    sec = sections(elf)
    data = sec.get(".data", (0, 0))[0]
    bss = sec.get(".bss", (0, 0))[0]
    ram2 = sec.get(".ram2", (0, 0))[0]
    # The stack is everything below .data (flip-link).
    stack = sec[".data"][1] - RAM_START if ".data" in sec else 0
    syms = symbols(elf)
    pool = sum(s for s, _, _, n in syms if n.endswith("__embassy_main::POOL"))

    print(f"flash  .text {sec['.text'][0]}  .rodata {sec['.rodata'][0]}")
    print(f"RAM    .data {data}  .bss {bss}  (main future POOL {pool})")
    print(f"stack  {stack}")
    print(f"RAM2   .ram2 {ram2} of {RAM2[1] - RAM2[0]}")

    print(f"\nbiggest statics:")
    ram = [s for s in syms if s[2] in "bBdD" and s[1] >= RAM_START]
    for size, addr, _, name in sorted(ram, reverse=True)[:TOP]:
        where = ".ram2" if addr >= RAM2[0] else ""
        print(f"  {size:6}  {where:5}  {short(name)}")

    print(f"\nbiggest stack frames:")
    for n, f in frames(elf)[:TOP]:
        print(f"  {n:6}  {short(f)}")


if __name__ == "__main__":
    main()
