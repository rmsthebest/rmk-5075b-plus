# wb32-pac

Register access for the WestberryTech **WB32FQ95** (Cortex-M3), in the
[chiptool](https://github.com/embassy-rs/chiptool) style that embassy-stm32 uses:
- one block struct per peripheral over a pointer, e.g. `pub const GPIOA: gpio::Gpio`;
- `Reg<T, RW|R|W>` accessors;
- newtype field sets in `regs::*`, with `x()` and `set_x()`.

```rust
use wb32_pac::{GPIOC, gpio};
GPIOC.bsrr().write_value(gpio::regs::Bsrr(1 << 5));
let set = GPIOC.odr().read().0 & (1 << 5) != 0;
```

## What it covers
- All 25 peripheral blocks of the vendor CMSIS header `wb32fq95xx.h` (from ChibiOS-contrib), and
  every instance.
- The interrupt enum: 37 vectors, the same as the reference manual (RM2905025 §9.2) for every line
  used so far.
- `NVIC_PRIO_BITS = 4`.
- With the `rt` feature: the vector table, `device.x` for cortex-m-rt, and the `interrupt`
  attribute.

## What it doesn't
- It's generated from the C header, not from an SVD, so field coverage is only what the header
  defines. Many registers have no named fields and are used through `.0`.
- No enums for field values, no metadata, no defmt.
- It covers only the WB32FQ95. Other WB32 parts (e.g. the WB32F10x) are not included.

## How it's made and checked
`tools/gen_pac.py` writes `src/lib.rs` and `device.x` from the header. The fixes and field arrays
come from `tools/pac-overrides.toml`. `tools/pac_vs_rm.py` compares the result with the reference
manual, field by field. Don't edit `src/lib.rs` by hand: change the overrides and regenerate. A
regeneration must be byte-identical unless the overrides changed.
