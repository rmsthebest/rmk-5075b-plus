# sim-rs: our own Akko 5075B Plus simulator

A Cortex-M3 interpreter and WB32FQ95 models in Rust, with the tests as plain
`cargo test`. It replaces the Renode sim in `../sim` once both agree on every
test; until then Renode is still the gate for firmware images.

```fish
sim-rs/run.sh                   # build and snapshot the images, run every test
sim-rs/run.sh -p wb32-sim       # cargo test arguments pass through
cd sim-rs; cargo test           # against the last snapshot
```

`run.sh` builds the firmware in `target/sim-rs*` (not the repo's `target/`, so a
Renode run can go on at the same time) and copies the images to
`sim-rs/target/images/`, which is all the tests read. `AKKO_SIM_IMAGES=` points
them at another directory.

## Crates

| Crate | What |
|---|---|
| `cortex-m3-emu` | The core: Thumb-2 interpreter, NVIC, SysTick, SCB, DWT. Knows nothing about the chip. |
| `wb32-sim` | The WB32FQ95: memory map, image loading, peripheral models. |
| `akko-sim` | The board (ROM stub, key matrix, radio, LEDs, SPI flash, USB host) and the test harness. |

## References

The simulator checks our PAC, HAL and firmware, so it must not be built from
them. Its references are the Renode sim (its models were checked against the
QMK binary and its golden traces) and the keyboard itself:
- No crate here depends on `wb32-pac`, `wb32-hal` or any other crate of the repo's; `crates/akko-sim/tests/deps.rs` checks `Cargo.lock`.
- Register facts come from `../sim/peripherals/*.py`, the vendor `wb32fq95xx.h` and the reference manual (RM2905025), and each model says which.
- Where the board and a model disagree, the board wins, recorded in [BOARD_FACTS.md](BOARD_FACTS.md).

## Status

- Step 1: the workspace, the memory map (flash at 0 and 0x0800_0000, ROM, SRAM), ELF and binary loading, the ROM stub. Accesses to nothing, and stores to flash or ROM, are recorded as `Diag`s rather than faulting, as Renode logs them.
- Step 2: the core. The decoder agrees with llvm-objdump on every instruction of the images, the ROM and a random blob (`tests/decode_oracle.rs`); the interpreter, exceptions, NVIC, SysTick, SCB and DWT have their own tests (`cortex-m3-emu/tests/cpu.rs`). Timing is Renode's: 100 MIPS, SysTick and CYCCNT at 96 MHz. A sleeping core jumps to the next event.

Next: check the core against Renode's execution traces of the same images (step 3).
