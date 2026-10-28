#!/usr/bin/env python3
"""Generate the wb32-pac crate from ChibiOS-contrib's CMSIS header.

    tools/gen_pac.py <wb32fq95xx.h> [--review BLOCK ...]

Writes crates/wb32-pac/src/lib.rs and crates/wb32-pac/device.x. The API follows chiptool's
(as used by embassy-stm32): `pac::RCC.apb1enr().modify(|w| w.set_gpioaen(true))`.

- Register blocks and offsets come from the `typedef struct ... X_TypeDef`
  layouts. Every offset is cross-checked against the header's own
  "Address offset: 0x..." comments, and generation fails on a mismatch.
- Instances and base addresses come from `#define GPIOA ((GPIO_TypeDef *) ...)`
  and the `*_BASE` defines; interrupts from `IRQn_Type`.
- Fields come from the bit defines, by these rules:
    X_Msk                          authoritative field X
    X  (0xN << P), N = 2^k - 1     field X
    X_<anything> under an X_Msk    enumerated value of X: skipped
    X_<digits> when X is a field   sub-bit of X: skipped
    no shift, e.g. (0x2U)          a value, not a field: skipped
  Overlapping fields are dropped and reported. tools/pac-overrides.toml
  replaces the fields of any register (e.g. GPIO/EXTI as indexed arrays).
- --review prints the fields of the given blocks, for checking by eye.
"""

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/wb32-pac"
OVERRIDES = ROOT / "tools" / "pac-overrides.toml"

TYPES = {"uint8_t": 1, "uint16_t": 2, "uint32_t": 4}
ACCESS = {"__IOM": "RW", "__IM": "R", "__OM": "W", "__IO": "RW", "__I": "R", "__O": "W"}


def die(msg):
    sys.exit(f"gen_pac: {msg}")


# ---------------------------------------------------------------- struct layouts

class Reg:
    def __init__(self, name, size, access, count, offset):
        self.name, self.size, self.access, self.count, self.offset = name, size, access, count, offset
        self.comment_off = None


class Cluster:
    def __init__(self, name, members, size, count, offset):
        self.name, self.members, self.size, self.count, self.offset = name, members, size, count, offset


def strip_comments_keep_offsets(body):
    """Return [(code, first_offset_from_comment_or_None)] per line."""
    out = []
    for line in body.splitlines():
        m = re.search(r"Address offset:\s*(0x[0-9A-Fa-f]+)", line)
        off = int(m.group(1), 16) if m else None
        if off is None:  # reserved lines carry a bare "0x010" or "0x050 - 0x058"
            m = re.search(r"/\*.*?(0x[0-9A-Fa-f]+)", line)
            off = int(m.group(1), 16) if m and "Reserved" in line else None
        code = re.sub(r"/\*.*?\*/", " ", line)
        code = re.sub(r"//.*", " ", code)
        out.append((code, off))
    return out


def tokenize_members(body):
    """Split a struct body into top-level declarations, keeping the offset comment
    of the line each declaration starts on (first declaration per line only)."""
    decls, cur, depth, cur_off, line_used = [], "", 0, None, False
    for code, off in strip_comments_keep_offsets(body):
        line_used = False
        for ch in code:
            if not cur.strip() and not ch.isspace() and not line_used:
                cur_off, line_used = off, True
            elif not cur.strip() and not ch.isspace():
                cur_off = None
            cur += ch
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
            elif ch == ";" and depth == 0:
                decls.append((cur.strip().rstrip(";").strip(), cur_off))
                cur, cur_off = "", None
        cur += "\n"
    if cur.strip():
        die(f"trailing text in struct: {cur.strip()[:60]}")
    return decls


def align(x, a):
    return (x + a - 1) // a * a


def parse_members(body, base=0):
    members, offset, max_align = [], 0, 1
    for decl, comment_off in tokenize_members(body):
        if decl.startswith("union"):
            inner = decl[decl.index("{") + 1:decl.rindex("}")]
            alts, _, a = parse_members(inner, None)  # sequential layout; re-placed below
            offset = align(offset, a)
            for alt in alts:  # every member overlaps at the union's offset
                alt.offset = offset
                check_offset(alt.name, base + offset, alt.comment_off)
                members.append(alt)
            offset += max(alt.size * alt.count for alt in alts)
            max_align = max(max_align, a)
            continue
        if decl.startswith("struct"):
            inner = decl[decl.index("{") + 1:decl.rindex("}")]
            m = re.fullmatch(r"(\w+)\s*(?:\[(\d+)\])?", decl[decl.rindex("}") + 1:].strip())
            sub, size, a = parse_members(inner, None if base is None else base + align(offset, 4))
            size = align(size, a)
            offset = align(offset, a)
            count = int(m.group(2) or 1)
            members.append(Cluster(m.group(1), sub, size, count, offset))
            offset += size * count
            max_align = max(max_align, a)
            continue
        m = re.fullmatch(r"(?:(__[IOM]+)\s+)?(?:const\s+)?(uint(?:8|16|32)_t)\s+(\w+)\s*(?:\[(\d+)\])?", decl)
        if not m:
            die(f"can't parse member: {decl!r}")
        qual, ty, name, count = m.groups()
        size = TYPES[ty]
        offset = align(offset, size)
        count = int(count or 1)
        if qual is not None:
            if base is not None:
                check_offset(name, base + offset, comment_off)
            reg = Reg(name, size, ACCESS[qual], count, offset)
            reg.comment_off = comment_off
            members.append(reg)
        offset += size * count
        max_align = max(max_align, size)
    return members, offset, max_align


def check_offset(name, computed, commented):
    if commented is not None and commented != computed:
        die(f"{name}: computed offset 0x{computed:03X} but the header says 0x{commented:03X}")


def parse_structs(text):
    blocks = {}
    for m in re.finditer(r"typedef\s+struct\s*\{", text):
        start = m.end()
        depth, i = 1, start
        while depth:
            depth += {"{": 1, "}": -1}.get(text[i], 0)
            i += 1
        tail = re.match(r"\s*(\w+)_TypeDef\s*;", text[i:])
        if not tail:
            continue
        members, size, _ = parse_members(text[start:i - 1])
        blocks[tail.group(1)] = members
    return blocks


# ---------------------------------------------------------------- instances, IRQs

def c_int(expr, defines, depth=0):
    if depth > 10:
        die(f"can't evaluate {expr}")
    expr = re.sub(r"\(\s*(?:uint\d+_t|u?int)\s*\)", "", expr)
    expr = re.sub(r"\b(0x[0-9A-Fa-f]+|\d+)U?L?\b", r"\1", expr)
    expr = re.sub(r"\b[A-Za-z_][A-Za-z0-9_]*\b", lambda m: f"({c_int(defines[m.group(0)], defines, depth + 1)})", expr)
    return eval(expr, {"__builtins__": {}})


def parse_defines(text):
    return dict(re.findall(r"^#define\s+([A-Za-z_][A-Za-z0-9_]*)\s+(.+?)\s*(?:/\*.*|//.*)?$", text, re.M))


def parse_instances(text, defines, blocks):
    inst = {}
    for name, ty, base in re.findall(r"^#define\s+(\w+)\s+\(\(\s*(\w+)_TypeDef\s*\*\s*\)\s*(\w+)\s*\)", text, re.M):
        if ty in blocks:
            inst[name] = (ty, c_int(base, defines))
    return inst


def parse_irqs(text):
    body = re.search(r"typedef\s+enum(?:\s+IRQn)?\s*\{(.*?)\}\s*IRQn_Type", text, re.S).group(1)
    return [(n, int(v)) for n, v in re.findall(r"(\w+)_IRQn\s*=\s*(-?\d+)", body) if int(v) >= 0]


# ---------------------------------------------------------------- fields

def all_regs(members, prefix=""):
    for m in members:
        if isinstance(m, Cluster):
            yield from all_regs(m.members, prefix)
        else:
            yield m


def parse_mask(expr, defines):
    """Field mask from a define value, or None if it isn't written as a field."""
    e = re.sub(r"\(\s*uint\d+_t\s*\)", "", expr).replace(" ", "")
    m = re.fullmatch(r"\(*(0x[0-9A-Fa-f]+|\d+)U?<<(\w+)\)*", e)
    if not m:
        return None
    n = int(m.group(1), 0)
    shift = m.group(2)
    shift = int(shift) if shift.isdigit() else c_int(defines[shift], defines)
    return n << shift


def msk_value(expr, defines):
    try:
        return c_int(re.sub(r"\(\s*uint\d+_t\s*\)", "", expr), defines)
    except Exception:
        return None


def contiguous(mask):
    if mask <= 0:
        return None
    lo = (mask & -mask).bit_length() - 1
    width = (mask >> lo).bit_length()
    return (lo, width) if (mask >> lo) == (1 << width) - 1 else None


def derive_fields(block, members, defines, report):
    names = {r.name: r for r in all_regs(members)}
    for r in list(names.values()):
        for i in range(1, r.count + 1):  # AFIO_EXTICR1_* describe EXTICR[0]
            names.setdefault(f"{r.name}{i}", r if i == 1 else None)
    fields = {}  # reg name -> {field: (lo, width)}
    authoritative = {}
    candidates = {}
    for dname, val in defines.items():
        if not dname.startswith(block + "_") or dname.endswith("_Pos"):
            continue
        rest = dname[len(block) + 1:]
        reg = next((n for n in sorted(names, key=len, reverse=True) if rest.startswith(n + "_")), None)
        if reg is None or names[reg] is None:
            continue
        fname = rest[len(reg) + 1:]
        target = names[reg].name
        if fname.endswith("_Msk"):
            span = contiguous(msk_value(val, defines) or 0)
            if span:
                authoritative.setdefault(target, {})[fname[:-4]] = span
            continue
        mask = parse_mask(val, defines)
        span = contiguous(mask or 0)
        if span:
            candidates.setdefault(target, {})[fname] = span
    for reg in set(authoritative) | set(candidates):
        auth = authoritative.get(reg, {})
        f = dict(auth)
        for name, span in candidates.get(reg, {}).items():
            if name in f:
                continue
            parent = next((a for a in list(auth) + list(candidates[reg]) if name.startswith(a + "_")), None)
            if parent is not None:
                continue  # enum value or sub-bit of a field
            f[name] = span
        # drop overlaps
        bad = set()
        items = list(f.items())
        for i, (a, (alo, aw)) in enumerate(items):
            for b, (blo, bw) in items[i + 1:]:
                if alo < blo + bw and blo < alo + aw:
                    bad |= {a, b}
        for b in sorted(bad):
            report.append(f"{block}.{reg}: dropped overlapping field {b}")
            del f[b]
        fields[reg] = {n: (lo, w, None) for n, (lo, w) in f.items()}
    return fields


# ---------------------------------------------------------------- Rust output

def snake(s):
    return s.lower()


def camel(s):
    return "".join(p[:1].upper() + p[1:].lower() for p in s.split("_") if p)


def rust_ident(s):
    s = snake(s)
    if s[0].isdigit():
        s = "_" + s
    return s + "_" if s in {"type", "ref", "loop", "match", "move", "in", "fn", "impl", "use", "mod", "static", "const", "box", "async", "await", "dyn"} else s


def field_ty(width, bits):
    if width == 1:
        return "bool"
    return "u8" if width <= 8 else "u16" if width <= 16 else "u32"


def gen_fieldset(name, bits, flds, doc):
    raw = {8: "u8", 16: "u16", 32: "u32"}[bits]
    out = [f"    #[doc = {doc!r}]".replace("'", '"'),
           "    #[repr(transparent)]",
           "    #[derive(Copy, Clone, Eq, PartialEq)]",
           f"    pub struct {name}(pub {raw});",
           f"    impl {name} {{"]
    for fname, (lo, width, arr) in sorted(flds.items(), key=lambda kv: kv[1][0]):
        ty = field_ty(width, bits)
        m = (1 << width) - 1
        ident = rust_ident(fname)
        setter = "set_" + ident.lstrip("_")
        get = "val != 0" if ty == "bool" else f"val as {ty}"
        put = "val as " + raw
        if arr is None:
            out += [
                "        #[inline(always)]",
                f"        pub const fn {ident}(&self) -> {ty} {{",
                f"            let val = (self.0 >> {lo}usize) & 0x{m:x};",
                f"            {get}",
                "        }",
                "        #[inline(always)]",
                f"        pub fn {setter}(&mut self, val: {ty}) {{",
                f"            self.0 = (self.0 & !(0x{m:x} << {lo}usize)) | ((({put}) & 0x{m:x}) << {lo}usize);",
                "        }",
            ]
        else:
            n, stride = arr
            out += [
                "        #[inline(always)]",
                f"        pub const fn {ident}(&self, n: usize) -> {ty} {{",
                f"            assert!(n < {n}usize);",
                f"            let offs = {lo}usize + n * {stride}usize;",
                f"            let val = (self.0 >> offs) & 0x{m:x};",
                f"            {get}",
                "        }",
                "        #[inline(always)]",
                f"        pub fn {setter}(&mut self, n: usize, val: {ty}) {{",
                f"            assert!(n < {n}usize);",
                f"            let offs = {lo}usize + n * {stride}usize;",
                f"            self.0 = (self.0 & !(0x{m:x} << offs)) | ((({put}) & 0x{m:x}) << offs);",
                "        }",
            ]
    out += ["    }",
            f"    impl Default for {name} {{",
            "        #[inline(always)]",
            "        fn default() -> Self {",
            f"            Self(0)",
            "        }",
            "    }",
            f"    impl core::fmt::Debug for {name} {{",
            "        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {",
            f"            write!(f, \"{name}({{:#x}})\", self.0)",
            "        }",
            "    }"]
    return out


def gen_accessors(members, fieldset_name, indent="        "):
    out = []
    for m in members:
        if isinstance(m, Cluster):
            ty = camel(m.name)
            if m.count > 1:
                out += [f"{indent}#[inline(always)]",
                        f"{indent}pub const fn {rust_ident(m.name)}(self, n: usize) -> {ty} {{",
                        f"{indent}    assert!(n < {m.count}usize);",
                        f"{indent}    unsafe {{ {ty}::from_ptr(self.ptr.wrapping_add(0x{m.offset:x}usize + n * 0x{m.size:x}usize) as _) }}",
                        f"{indent}}}"]
            else:
                out += [f"{indent}#[inline(always)]",
                        f"{indent}pub const fn {rust_ident(m.name)}(self) -> {ty} {{",
                        f"{indent}    unsafe {{ {ty}::from_ptr(self.ptr.wrapping_add(0x{m.offset:x}usize) as _) }}",
                        f"{indent}}}"]
            continue
        fs = fieldset_name(m)
        for name in [m.name]:
            doc = f"{m.name} @ 0x{m.offset:03X}"
            if m.count > 1:
                out += [f"{indent}#[doc = \"{doc}\"]",
                        f"{indent}#[inline(always)]",
                        f"{indent}pub const fn {rust_ident(name)}(self, n: usize) -> crate::common::Reg<regs::{fs}, crate::common::{m.access}> {{",
                        f"{indent}    assert!(n < {m.count}usize);",
                        f"{indent}    unsafe {{ crate::common::Reg::from_ptr(self.ptr.wrapping_add(0x{m.offset:x}usize + n * {m.size}usize) as _) }}",
                        f"{indent}}}"]
            else:
                out += [f"{indent}#[doc = \"{doc}\"]",
                        f"{indent}#[inline(always)]",
                        f"{indent}pub const fn {rust_ident(name)}(self) -> crate::common::Reg<regs::{fs}, crate::common::{m.access}> {{",
                        f"{indent}    unsafe {{ crate::common::Reg::from_ptr(self.ptr.wrapping_add(0x{m.offset:x}usize) as _) }}",
                        f"{indent}}}"]
    return out


def gen_block_struct(name, members, fieldset_name, doc):
    out = [f"    #[doc = \"{doc}\"]",
           "    #[derive(Copy, Clone, Eq, PartialEq)]",
           f"    pub struct {name} {{",
           "        ptr: *mut u8,",
           "    }",
           f"    unsafe impl Send for {name} {{}}",
           f"    unsafe impl Sync for {name} {{}}",
           f"    impl {name} {{",
           "        #[inline(always)]",
           "        pub const unsafe fn from_ptr(ptr: *mut ()) -> Self {",
           "            Self { ptr: ptr as _ }",
           "        }",
           "        #[inline(always)]",
           "        pub const fn as_ptr(&self) -> *mut () {",
           "            self.ptr as _",
           "        }"]
    out += gen_accessors(members, fieldset_name)
    out += ["    }"]
    for m in members:
        if isinstance(m, Cluster):
            out += gen_block_struct(camel(m.name), m.members, fieldset_name, f"{m.name} cluster")
    return out


COMMON = r'''
pub mod common {
    //! Register access, as in chiptool-generated PACs.
    use core::marker::PhantomData;
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct RW;
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct R;
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct W;
    pub trait Access: Copy {}
    impl Access for R {}
    impl Access for W {}
    impl Access for RW {}
    pub trait Read: Access {}
    impl Read for RW {}
    impl Read for R {}
    pub trait Write: Access {}
    impl Write for RW {}
    impl Write for W {}

    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct Reg<T: Copy, A: Access> {
        ptr: *mut u8,
        phantom: PhantomData<*mut (T, A)>,
    }
    unsafe impl<T: Copy, A: Access> Send for Reg<T, A> {}
    unsafe impl<T: Copy, A: Access> Sync for Reg<T, A> {}

    impl<T: Copy, A: Access> Reg<T, A> {
        #[inline(always)]
        pub const unsafe fn from_ptr(ptr: *mut T) -> Self {
            Self { ptr: ptr as _, phantom: PhantomData }
        }
        #[inline(always)]
        pub const fn as_ptr(&self) -> *mut T {
            self.ptr as _
        }
    }
    impl<T: Copy, A: Read> Reg<T, A> {
        #[inline(always)]
        pub fn read(&self) -> T {
            unsafe { (self.ptr as *mut T).read_volatile() }
        }
    }
    impl<T: Copy, A: Write> Reg<T, A> {
        #[inline(always)]
        pub fn write_value(&self, val: T) {
            unsafe { (self.ptr as *mut T).write_volatile(val) }
        }
    }
    impl<T: Default + Copy, A: Write> Reg<T, A> {
        #[inline(always)]
        pub fn write<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
            let mut val = Default::default();
            let res = f(&mut val);
            self.write_value(val);
            res
        }
    }
    impl<T: Copy, A: Read + Write> Reg<T, A> {
        #[inline(always)]
        pub fn modify<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
            let mut val = self.read();
            let res = f(&mut val);
            self.write_value(val);
            res
        }
    }
}
'''


def build_model(header):
    """(blocks, instances, irqs, fields per used block, report) from the
    header, with tools/pac-overrides.toml applied: what lib.rs is generated
    from (tools/pac_vs_rm.py checks it against the reference manual)."""
    text = Path(header).read_text(errors="replace")
    defines = parse_defines(text)
    blocks = parse_structs(text)
    instances = parse_instances(text, defines, blocks)
    irqs = parse_irqs(text)
    overrides = tomllib.loads(OVERRIDES.read_text()) if OVERRIDES.exists() else {}
    report, fields_by_ty = [], {}
    for ty in sorted({ty for ty, _ in instances.values()}):
        fields = derive_fields(ty, blocks[ty], defines, report)
        for reg, ov in overrides.get(ty, {}).items():
            fields[reg] = {}
            for fname, spec in ov.items():
                arr = (spec["len"], spec["stride"]) if "len" in spec else None
                fields[reg][fname] = (spec["bit"], spec["width"], arr)
        fields_by_ty[ty] = fields
    return blocks, instances, irqs, fields_by_ty, report


def main():
    args = sys.argv[1:]
    if not args:
        die("usage: gen_pac.py <wb32fq95xx.h> [--review BLOCK ...]")
    header = Path(args[0])
    review = args[args.index("--review") + 1:] if "--review" in args else []
    blocks, instances, irqs, fields_by_ty, report = build_model(header)
    used = sorted(fields_by_ty)
    lines = ["//! WB32FQ95 peripheral access crate. GENERATED by tools/gen_pac.py from",
             f"//! {header.name}; edit the generator or tools/pac-overrides.toml, not this file.",
             "#![no_std]",
             "#![allow(clippy::all, non_camel_case_types, unused)]",
             COMMON]

    for ty in used:
        members = blocks[ty]
        fields = fields_by_ty[ty]

        regs_out, seen = [], {}

        def fieldset_name(r, _seen=seen, _out=regs_out, _fields=fields):
            name = camel(r.name)
            if name in _seen:
                return name
            _seen[name] = True
            _out.extend(gen_fieldset(name, r.size * 8, _fields.get(r.name, {}), f"{ty} {r.name}"))
            return name

        mod = snake(ty)
        lines.append(f"pub mod {mod} {{")
        lines += gen_block_struct(camel(ty), members, fieldset_name, f"{ty} register block")
        lines.append("    pub mod regs {")
        lines += ["    " + l for l in regs_out]
        lines.append("    }")
        lines.append("}")

        if ty in review:
            print(f"== {ty}")
            for r in all_regs(members):
                f = fields.get(r.name, {})
                desc = ", ".join(f"{n}[{lo}{'' if w == 1 else ':' + str(lo + w - 1)}]{'x' + str(a[0]) if a else ''}"
                                 for n, (lo, w, a) in sorted(f.items(), key=lambda kv: kv[1][0]))
                print(f"  0x{r.offset:03X} {r.name:<14} {r.access:<2} {desc or '(raw)'}")

    for name, (ty, base) in sorted(instances.items(), key=lambda kv: kv[1][1]):
        lines.append(f"#[doc = \"{name} @ 0x{base:08X}\"]")
        lines.append(f"pub const {name}: {snake(ty)}::{camel(ty)} = unsafe {{ {snake(ty)}::{camel(ty)}::from_ptr(0x{base:08x}usize as _) }};")

    # interrupts
    n_irq = max(v for _, v in irqs) + 1
    lines += ["#[derive(Copy, Clone, Debug, PartialEq, Eq)]",
              "#[allow(non_camel_case_types)]",
              "#[repr(u16)]",
              "pub enum Interrupt {"]
    lines += [f"    {n} = {v}," for n, v in irqs]
    lines += ["}",
              "unsafe impl cortex_m::interrupt::InterruptNumber for Interrupt {",
              "    #[inline(always)]",
              "    fn number(self) -> u16 {",
              "        self as u16",
              "    }",
              "}",
              "pub const NVIC_PRIO_BITS: u8 = 4;",
              "#[cfg(feature = \"rt\")]",
              "mod _vectors {",
              "    unsafe extern \"C\" {"]
    lines += [f"        fn {n}();" for n, _ in irqs]
    lines += ["    }",
              "    pub union Vector {",
              "        _handler: unsafe extern \"C\" fn(),",
              "        _reserved: u32,",
              "    }",
              "    #[unsafe(link_section = \".vector_table.interrupts\")]",
              "    #[unsafe(no_mangle)]",
              f"    pub static __INTERRUPTS: [Vector; {n_irq}] = ["]
    by_num = dict((v, n) for n, v in irqs)
    for i in range(n_irq):
        lines.append(f"        Vector {{ _handler: {by_num[i]} }}," if i in by_num else "        Vector { _reserved: 0 },")
    lines += ["    ];", "}",
              "#[cfg(feature = \"rt\")]",
              "pub use cortex_m_rt::interrupt;",
              "pub use Interrupt as interrupt;"]

    (OUT / "src").mkdir(parents=True, exist_ok=True)
    (OUT / "src" / "lib.rs").write_text("\n".join(lines) + "\n")
    (OUT / "device.x").write_text("".join(f"PROVIDE({n} = DefaultHandler);\n" for n, _ in irqs))
    for r in report:
        print(r, file=sys.stderr)
    print(f"gen_pac: {len(used)} blocks, {len(instances)} instances, {len(irqs)} interrupts", file=sys.stderr)


if __name__ == "__main__":
    main()
