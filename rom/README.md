# rom/: the WB32FQ95 ROM bootloader, read and rewritten

The chip's ROM DFU bootloader (system memory, 0x1FFFE000, 4 KB) is the one part of this keyboard we don't control. This directory holds what we built to understand it:
- the tools that read it out and disassemble it;
- the write-up, `notes.md`: the boot sequence, the USB command protocol, flash programming, read protection and the updater's `-t`;
- the Rust translation (step 4, `rust/`).

Most people won't need any of this to build or flash the firmware.

**The ROM itself is not in this repo.** It's Westberry's code, and the information-block dump holds the chip's unique ID. Both stay in a work folder outside the repo, `$ROM_WORK`, which defaults to `../wb32-rom-re` beside this checkout:

```
$ROM_WORK/dump/            rom.bin, info.bin, SHA256SUMS (rom/tools/rompeek.py)
$ROM_WORK/out/             labels, listings, Ghidra exports (decompiled vendor code)
$ROM_WORK/ghidra-project/  the Ghidra project
```

**Read-only.** Nothing here writes to the keyboard. The ROM can't be written: no ROM command reaches it (notes.md). The information blocks (boot control, write protection, OTP) can be written, and we never do. Never run `wb32-dfu-updater_cli -t`: it erases and rewrites the protection block (notes.md "Toolbox mode"). The Rust translation runs in the simulator only and never goes on the board.

## Reading the ROM out

There's no SWD on this board, so the running firmware reads it, over Vial:

```fish
cargo build --release --bin akko-rmk --features rom-peek   # Vial channel 0xDA: read-only peek
# freeze and flash it as any image (sim gate, tools/precheck.py, bench), then:
python3 -I rom/tools/rompeek.py                            # → $ROM_WORK/dump/
```

The peek reads only 0x1FFFE000–0x1FFFEFFF and 0x1FFFF000–0x1FFFF7FF. Everything is read twice and must agree. Built without the feature, the image is trunk's: the code is gated out.

## Layout

| Path | What |
|---|---|
| `tools/rompeek.py` | Reads the dump through the `rom-peek` image |
| `tools/gen_labels.py` | Register names from wb32-pac and vector names from `target/rm-regs.json` → `$ROM_WORK/out/` |
| `tools/disasm.py` | objdump listing, with literal-pool words resolved to register names |
| `tools/info.py` | Decodes `info.bin` block by block (`--uid` to show the unique ID) |
| `tools/ghidra.sh`, `ghidra/scripts/` | Headless Ghidra 12: memory map, vectors, labels, then the decompiled C, call graph, MMIO access map and outside-ROM references |
| `notes.md` | What the ROM does, with ROM addresses for every claim |
| `rust/` | `wb32-rom-dfu`, the ROM rewritten in Rust; the same write traces as the ROM (`rust/README.md`) |

## Working on it

```fish
python3 -I rom/tools/gen_labels.py
python3 -I rom/tools/disasm.py         # $ROM_WORK/out/rom.lst
python3 -I rom/tools/info.py
rom/tools/ghidra.sh                    # $ROM_WORK/out/decomp.c etc.
sim-rs/run.sh -p akko-sim --test rom_sim --test rom_traces --test peek
```

The real ROM runs in the board simulator (`sim-rs`): `rom_sim.rs` enumerates it, downloads through it and leaves it, `rom_traces.rs` holds its register writes to the traces in `sim-rs/golden/rom/` (recorded in the Renode sim this replaced), and `peek.rs` checks the `rom-peek` channel. Both run on the dump and on `rust/`'s build. The ROM's FMC and SYS models are in `sim-rs/crates/wb32-sim/src/fmc.rs`; SYS MEMSZ 0x03 is an **assumption** (the value that gives the 256 KB / 36 KB the vendor tool reports).

`disasm.py` and `ghidra.sh` take `--rom FILE --base ADDR` to run on other images. For example, `--rom ../stock_5075.bin --base 0x08000000` takes the first 4 KB of the stock firmware.
