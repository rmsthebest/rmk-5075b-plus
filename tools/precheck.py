#!/usr/bin/env python3
"""Refuse to flash an image that could lock us out of the ROM bootloader.

The keyboard can only reach DFU through our own reset code (no SWD, no
BOOT0), so every image is checked before it goes near the keyboard:

  - vector table: SP in RAM below the magic word, Reset/HardFault/etc. are
    Thumb addresses in flash, HardFault and DefaultHandler are boot-guard's
  - Reset calls boot-guard's __pre_init first
  - the boot-guard code matches a reviewed fingerprint (tools/boot-guard.lock)
  - .data/.bss/stack stay clear of the magic word, image fits in flash

Usage: precheck.py <elf> <bin> [--accept-stub]
  --accept-stub  record the current boot-guard fingerprint as reviewed.
                 Only after reading the disassembly it prints.
"""

import hashlib
import re
import struct
import subprocess
import sys
from pathlib import Path

FLASH = (0x0800_0000, 256 * 1024)  # WB32FQ95xC
RAM = (0x2000_0000, 28 * 1024)
MAGIC_ADDR = 0x2000_6FFC
GUARD_SYMBOLS = [
    "__pre_init",
    "__boot_guard_enter_rom",
    "__boot_guard_reset_to_dfu",
    "HardFault",
    "DefaultHandler",
    "__boot_guard_pool",
    "__boot_guard_pre_init_pool",
]
# Literal pool of __boot_guard_enter_rom/__boot_guard_reset_to_dfu, in order.
POOL = [0x1FFF_E000, MAGIC_ADDR, 0xDEAD_BEEF, 0xE000_ED0C, 0x05FA_0004]
# Literal pool of the full __pre_init: magic, PWR, RCC, GPIOC (GPIOA and the
# small masks are encoded as immediates and covered by the fingerprint).
PRE_INIT_POOL = [MAGIC_ADDR, 0xDEAD_BEEF, 0x4001_0000, 0x4001_0C00, 0x4000_0800]
LOCK = Path(__file__).with_name("boot-guard.lock")


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def symbols(elf):
    out = {}
    for line in run("rust-nm", elf).splitlines():
        parts = line.split()
        if len(parts) == 3:
            out[parts[2]] = int(parts[0], 16)
    return out


def guard_listing(elf, variant):
    """Disassembly of the boot-guard code with layout-dependent parts removed."""
    text = run(
        "rust-objdump", "-d", "--no-show-raw-insn", "--no-leading-addr",
        "--disassemble-symbols=" + ",".join(GUARD_SYMBOLS), elf,
    )
    lines = [f"variant: {variant}"]
    for line in text.splitlines():
        line = line.split("@")[0].rstrip()
        # Data words in literal pools come with address and raw bytes.
        line = re.sub(r"^\s*[0-9a-f]+:\s+(?:[0-9a-f]{2} )+\s*", "", line)
        # Branch targets: keep the symbol, drop the absolute address.
        line = re.sub(r"0x[0-9a-f]+ <([^>+]+)(\+0x[0-9a-f]+)?>",
                      lambda m: f"<{m.group(1)}{m.group(2) or ''}>", line)
        # Branch immediates encode the same targets; drop them too.
        line = re.sub(r"\s+@?\s*imm = #-?0x[0-9a-f]+", "", line)
        if line.strip() and "file format" not in line and "Disassembly" not in line:
            lines.append(line.strip())
    # The last pool runs on to the next symbol, so it may end in lld's
    # alignment fill, depending on what the image links after it.
    while lines[-1] == ".word\t0xd4d4d4d4":
        lines.pop()
    return "\n".join(lines) + "\n"


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    elf, binp = sys.argv[1], sys.argv[2]
    accept = "--accept-stub" in sys.argv
    errors = []

    def check(cond, msg):
        if not cond:
            errors.append(msg)

    data = Path(binp).read_bytes()
    syms = symbols(elf)
    in_flash = lambda a: FLASH[0] <= a < FLASH[0] + len(data)

    check(len(data) <= FLASH[1], f"image is {len(data)} bytes, flash is {FLASH[1]}")
    vec = struct.unpack_from("<16I", data)

    sp = vec[0]
    check(RAM[0] < sp <= MAGIC_ADDR - 4 and sp % 8 == 0,
          f"initial SP {sp:#010x} must be 8-aligned, in RAM, below the magic word")

    for i, name in [(1, "Reset"), (2, "NMI"), (3, "HardFault"), (4, "MemManage"),
                    (5, "BusFault"), (6, "UsageFault"), (11, "SVCall"),
                    (14, "PendSV"), (15, "SysTick")]:
        v = vec[i]
        check(v & 1 and in_flash(v & ~1),
              f"vector {i} ({name}) {v:#010x} is not a Thumb address in the image")

    for name in [n for n in GUARD_SYMBOLS if n != "__boot_guard_pre_init_pool"] + ["Reset"]:
        check(name in syms, f"symbol {name} missing (is boot-guard linked?)")
    if errors:
        report(errors)

    check(vec[1] & ~1 == syms["Reset"], "reset vector does not point at Reset")
    check(vec[3] & ~1 == syms["HardFault"], "HardFault vector is not boot-guard's")
    for i in (2, 4, 5, 6, 11, 12, 14, 15):
        v = vec[i]
        if v:
            check(v & ~1 in (syms["DefaultHandler"], syms["HardFault"])
                  or i in (11, 14, 15),
                  f"vector {i} {v:#010x} is not a boot-guard handler")

    pool = syms["__boot_guard_pool"] - FLASH[0]
    words = list(struct.unpack_from(f"<{len(POOL)}I", data, pool))
    check(words == POOL, "boot-guard constants wrong: "
          + " ".join(f"{w:#010x}" for w in words))

    if "__boot_guard_pre_init_pool" in syms:
        pool = syms["__boot_guard_pre_init_pool"] - FLASH[0]
        words = list(struct.unpack_from(f"<{len(PRE_INIT_POOL)}I", data, pool))
        check(words == PRE_INIT_POOL, "boot-guard __pre_init constants wrong: "
              + " ".join(f"{w:#010x}" for w in words))

    # Reset's first instruction must be `bl __pre_init`.
    first = run("rust-objdump", "-d", "--no-show-raw-insn",
                "--disassemble-symbols=Reset", elf)
    insns = [l for l in first.splitlines() if re.match(r"\s+[0-9a-f]+:", l)]
    check(insns and re.search(r"\bbl\b.*<__pre_init>", insns[0]),
          f"Reset does not start with bl __pre_init: {insns[:1]}")

    # RAM layout: nothing may reach the magic word.
    for name in ("__ebss", "__edata", "_stack_start", "__sheap"):
        if name in syms:
            check(syms[name] <= MAGIC_ADDR,
                  f"{name} = {syms[name]:#010x} overlaps the magic word")
    for line in run("rust-size", "-A", "-x", elf).splitlines()[2:]:
        parts = line.split()
        if len(parts) == 3 and parts[2].startswith("0x2"):
            start, size = int(parts[2], 16), int(parts[1], 16)
            check(start + size <= MAGIC_ADDR,
                  f"section {parts[0]} ends at {start + size:#010x}, past the magic word")

    # The boot-guard code must be byte-for-byte what was reviewed.
    pre = run("rust-objdump", "-d", "--no-show-raw-insn",
              "--disassemble-symbols=__pre_init", elf)
    pre_insns = [l for l in pre.splitlines() if re.match(r"\s+[0-9a-f]+:", l)]
    variant = "always-dfu" if len(pre_insns) == 1 else "full"
    listing = guard_listing(elf, variant)
    digest = hashlib.sha256(listing.encode()).hexdigest()
    locked = {}
    if LOCK.exists():
        for line in LOCK.read_text().splitlines():
            if line.strip() and not line.startswith("#"):
                v, d = line.split()
                locked[v] = d
    if accept:
        print(listing)
        locked[variant] = digest
        LOCK.write_text("# reviewed boot-guard fingerprints (tools/precheck.py --accept-stub)\n"
                        + "".join(f"{v} {d}\n" for v, d in sorted(locked.items())))
        print(f"recorded {variant} {digest}")
    else:
        check(locked.get(variant) == digest,
              f"boot-guard ({variant}) changed or not reviewed: {digest}\n"
              "  review the disassembly, then rerun with --accept-stub")

    report(errors)


def report(errors):
    if errors:
        print("PRECHECK FAILED, do not flash:")
        for e in errors:
            print("  -", e)
        sys.exit(1)
    print("precheck ok")
    sys.exit(0)


if __name__ == "__main__":
    main()
