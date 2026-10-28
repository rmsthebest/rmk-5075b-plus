#!/usr/bin/env python3
"""Reads the register descriptions out of the WB32FQ95xx reference manual
(RM2905025, westberrytech.com; not in this repo), for tools/pac_vs_rm.py.

    tools/rm_regs.py <manual.pdf|manual.txt> [OUT.json]

A PDF goes through `pdftotext -layout` first. Each section headed
"N.N.N. <title> (BLOCK_REG)" becomes a register: its address offset, reset
value and bit table (bits, field name, access). The interrupt vector table
(section 9.2) comes out as "vectors". Sections whose heading names no
register, or with no offset, are listed under "skipped", so nothing is
dropped silently. The default OUT is target/rm-regs.json.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

HEADING = re.compile(r"^\s{0,16}(\d+\.\d+\.\d+)\.?\s+(\S.*?)\s*$")
# A table of contents line ends in dots and a page number.
TOC = re.compile(r"\.{4,}\s*\d+\s*$")
NAME = re.compile(r"[(（]\s*([A-Za-z][A-Za-z0-9_ ]*?(?:x?[A-Za-z0-9_]*)?)\s*(?:,[^)）]*)?[)）]")
OFFSET = re.compile(r"(?:Address\s+[Oo]ffset|Address\s+bias|Offset\s+address)\s*[:：]?\s*(0x[0-9A-Fa-f_ ]+)")
RESET = re.compile(r"Reset\s+value\s*[:：]?\s*(0x[0-9A-Fa-f_ ]+)")
ACCESS = r"(RW|R/W|RC_W0|RC_W1|R_W0|RS|WO|RO|W|R|Res)"
# "  31:4   -   R   Reserved", "  3   SRCEN   RW   ...", "[7:0] DATA RW".
FIELD = re.compile(r"^\s{0,10}\[?(\d{1,2})(?:\s*[:~]\s*(\d{1,2}))?\]?\s+([A-Za-z_][A-Za-z0-9_\[\]]*|-|Reserved)\s+"
                   + ACCESS + r"(?=\s|$)")
PAGE_FOOTER = re.compile(r"Doc ID|Reference Manual\s*$|Rev0\d")


def text_of(path):
    path = Path(path)
    if path.suffix.lower() == ".pdf":
        out = subprocess.run(["pdftotext", "-layout", str(path), "-"], capture_output=True, text=True, check=True)
        return out.stdout
    return path.read_text(errors="replace")


def hexval(s):
    return int(s.replace("_", "").replace(" ", ""), 16)


def split_name(raw):
    """'RCC_USBPRE' -> ('RCC', 'USBPRE'); 'GPIOx_MODER' -> ('GPIO', 'MODER');
    'PWR_ANAKEY 1' -> ('PWR', 'ANAKEY1'); 'TIMx_CR1' -> ('TIM', 'CR1')."""
    raw = raw.replace(" ", "")
    if "_" not in raw:
        return None, raw
    block, reg = raw.split("_", 1)
    block = re.sub(r"x$", "", block)
    return block.upper(), reg.upper()


def parse(text):
    lines = text.splitlines()
    sections = []
    cur = None
    for i, line in enumerate(lines):
        m = HEADING.match(line)
        if m and not TOC.search(line):
            cur = {"section": m.group(1), "title": m.group(2), "line": i + 1, "body": []}
            sections.append(cur)
            continue
        if cur is not None and not PAGE_FOOTER.search(line):
            cur["body"].append(line)

    regs, skipped = [], []
    for s in sections:
        body = "\n".join(s["body"])
        names = NAME.findall(s["title"])
        off = OFFSET.search(body)
        if not names or not off:
            if off or names:
                skipped.append({k: s[k] for k in ("section", "title", "line")})
            continue
        block, reg = split_name(names[-1])
        rst = RESET.search(body)
        fields = []
        for l in s["body"]:
            f = FIELD.match(l)
            if not f:
                continue
            hi = int(f.group(1))
            lo = int(f.group(2)) if f.group(2) is not None else hi
            hi, lo = max(hi, lo), min(hi, lo)
            name = f.group(3)
            if name in ("-", "Reserved") or f.group(4) == "Res":
                name = None
            fields.append({"name": name, "lo": lo, "width": hi - lo + 1, "access": f.group(4).replace("R/W", "RW")})
        regs.append({
            "section": s["section"], "title": s["title"], "line": s["line"],
            "block": block, "reg": reg,
            "offset": hexval(off.group(1)),
            "reset": hexval(rst.group(1)) if rst else None,
            "fields": fields,
        })
    return regs, skipped, vectors(lines)


def vectors(lines):
    """Section 9.2's table: (position, name) for the peripheral interrupts."""
    start = next((i for i, l in enumerate(lines) if re.match(r"^\s*9\.2\.\s+Interrupt and exception vectors", l)
                  and not TOC.search(l)), None)
    if start is None:
        return []
    out = []
    for l in lines[start:start + 400]:
        if re.match(r"^\s*10\.\s", l):
            break
        # "  0     7    settable   WWDG    Window watchdog interrupt   0x0000_0040"
        m = re.match(r"^\s*(\d{1,2})\s+\d{1,2}\s+settable\s+([A-Za-z0-9_]+)\s", l)
        if m:
            out.append((int(m.group(1)), m.group(2)))
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__.strip())
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / "target" / "rm-regs.json"
    regs, skipped, vecs = parse(text_of(sys.argv[1]))
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({"regs": regs, "skipped": skipped, "vectors": vecs}, indent=1))
    blocks = sorted({r["block"] or "?" for r in regs})
    print(f"rm_regs: {len(regs)} registers in {len(blocks)} blocks, {len(vecs)} vectors, "
          f"{len(skipped)} sections skipped -> {out}", file=sys.stderr)


if __name__ == "__main__":
    main()
