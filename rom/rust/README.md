# Step 4: `wb32-rom-dfu`, the ROM rewritten in Rust

The whole WB32FQ95 ROM bootloader (0x1FFFE000–0x1FFFEFFF, version 4) as a `no_std` Rust program, written from the disassembly (`$ROM_WORK/out/rom.lst`, `decomp.c`). Use it to read what the ROM does. **It runs in the Renode sim only and never goes on the board.** DFU can't write 0x1FFFE000 anyway, and no bench or flash script refers to it.

Every function names the ROM range it stands for (`/// ROM 0x1FFFE754-0x1FFFE7C7`). Register writes keep the ROM's order and its access widths, odd parts included (a register written twice, a 0xAAAA before 0x5555), with comments instead of tidying.

| Module | ROM | What |
|---|---|---|
| `main.rs` | 0x1FFFE000–0x1FFFE0FF, main's start | Vector table, reset handler, RAM init, the start-up order |
| `clock.rs` | 0x1FFFE940–0x1FFFEAD5, 0x1FFFE178, 0x1FFFE904 | Clocks to the 48 MHz PLL, the optional IWDG, the trim check, delay, watchdog feed |
| `usb.rs` | 0x1FFFE3D8–0x1FFFE4D7, 0x1FFFE624–0x1FFFE8AB, 0x1FFFED60–0x1FFFEE33 | USB init/deinit, pins, the forced disconnect, the interrupt, endpoint helpers, bulk receive/send |
| `ep0.rs` | 0x1FFFE1A0–0x1FFFE3D7, 0x1FFFE4DC–0x1FFFE623, 0x1FFFE718–0x1FFFE753 | Control transfers and the standard requests |
| `desc.rs` | 0x1FFFEF44–0x1FFFEFC6, 0x1FFFEE40 | Descriptors, memory-size tables |
| `dfu.rs` | 0x1FFFEAF2–0x1FFFEC8F, 0x1FFFE8CC, 0x1FFFE928 | The command loop: GET_INFO, ERASE, WRITE_PAGE, READ; the address and protection checks |
| `boot.rs` | 0x1FFFEC90–0x1FFFED19 | RESET and GO |
| `flash.rs` | 0x1FFFE104–0x1FFFE177, 0x1FFFEE54–0x1FFFEF43 | One FMC operation, run from the stack, and the FHSI check before it |
| `ram.rs` | SRAM 0x20000000–0x2000044F | The ROM's variables at its addresses (`link.x` checks them) |
| `mmio.rs` | | Register access at the ROM's widths, and the registers the PAC doesn't name |

Registers come from `crates/wb32-pac`. The values written are the ROM's raw constants.

## Build and check

```fish
cd rom/rust; cargo build --release        # its own target/, never the repo's
cd ../..
rom/sim/run.sh --rust                     # the ROM suites on this build: 9/9
python3 -I rom/sim/trace.py boot --image rom/rust/target/thumbv7m-none-eabi/release/wb32-rom-dfu --out /tmp/boot.txt
diff rom/sim/traces/boot.txt /tmp/boot.txt   # empty; the same for download and leave
```

**Equivalence:** all three write traces (boot 347 writes, download 605, leave 634) are identical to the ROM's. Matching them corrected two readings of the decompiled C. The differences that don't show in a trace are listed in ../notes.md "The Rust translation".

The crate is a workspace of its own. Cargo still merges in the repo's `.cargo/config.toml`, whose rustflags pass `-Tlink.x`, so the linker script is called `link.x` and `build.rs` puts it on the search path. The linker is plain `rust-lld`, not flip-link, so the stack stays where the ROM has it.

## License

MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`), unlike the rest of the repo (GPL-2.0-or-later). It's a reconstruction for reading. The ROM it describes is Westberry's and isn't in the repo.
