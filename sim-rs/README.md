# sim-rs: our own Akko 5075B Plus simulator

A Cortex-M3 interpreter and WB32FQ95 models in Rust, with the tests as plain
`cargo test`. It is the gate for every firmware image: the workbench's "Run
sim tests" (`../tools/bench.py`) runs the whole suite, about 100 s, and
freezes the akko-rmk it ran. The Renode sim in `../sim` runs the same suites
as a cross-check ("Cross-check in Renode"). The QMK build runs here too
(`qmk_reference.rs`, `qmk_power.rs`).

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

- Step 3: the core against Renode. `compare/renode-compare.sh <image> [secs]` records Renode's run (every instruction, every peripheral access) and steps our core through it in lockstep: peripheral reads get Renode's values, writes must match, interrupts are taken where Renode took them, and reads of the core's timers take their registers from snapshots Renode logs (`compare-renode` finds the PCs and asks for another run). It stops at the first difference. Every image matches: m1a, m1b and m3 (9.8M instructions, 0.1 s), m2 (0.5 s), akko-rmk and its mac build (11M instructions, 51k accesses, 0.3 s), and the ROM's boot from the dump (5M instructions, 454k accesses, 0.05 s). Our SysTick was pending at 51 of the 52 SysTick interrupts Renode took in akko-rmk; the other interrupts wait for the peripheral models.

- Step 4: the system controller (PWR, ANCTL with the ANAKEY lock, IWDG, RCC), the cache controller, GPIO with AFIO and EXTI and the key matrix, ported from the Renode models, and TIM2 written fresh to the board facts. `boot_guard.rs` and `m2_hal.rs` are the Renode suites ported test for test, and pass. `akko-sim trace <image> [secs] [regex]` prints the register writes as `../sim/trace.sh` does; `compare/trace-diff.sh` diffs the two. m1b and m2 match line for line over 0.5 s; m3 and akko-rmk match until their first access to a peripheral not modelled yet (USB, QSPI). Seeded model bugs (no EXTI falling edges, the PLL never ready) fail the ported tests.

- Step 5: the USB device controller (MUSB, with CRC/SFM), the scripted host (usb.py's host, with usb_rom.py's bulk pipe folded in; its transfers are async functions stepped every 100 us, as usb.py's generators), and the ROM sim's FMC and SYS. `m3_usb.rs` (15 tests, 1.2 s), `rom_sim.rs` (enumerate, download and leave: 9 tests on both the ROM dump and rom/rust's build) and `rom_traces.rs` (the `boot`, `download` and `leave` write traces in `../rom/sim/traces` match line for line, on both) pass. Two adjustments to Renode's traces, both in `rom_traces.rs`: its log folds a run of identical lines into two, and its AIRCR reads with reserved bit 13 set. m3's register-write trace matches `sim/trace.sh` over 0.5 s. Seeded bugs (a stuck host data toggle, the wrong USBPRE) fail the m3 tests. peek.robot comes with the SPI flash in step 6.

- Step 6a: the DesignWare SSI (QSPI and SPIM2 share it) and the 16550 UART3 in wb32-sim; the board's NOR flash (C12 chip select, kept across resets), WS2812 chain and radio module in akko-sim, as `SsiDevice`/`UartPeer`s ported from qspi.py, spim2.py and uart3.py. The host's log now runs on across a system reset, as Renode's does. `m4_rmk.rs` (29 tests, 17 s) and `peek.rs` (rom/sim's peek.robot, on akko-rmk built with `rom-peek`) pass. akko-rmk's register-write trace, flash commands included, matches `sim/trace.sh` over 8 s (7,005 writes). A seeded NOR bug (programming that doesn't clear bits) fails the five storage tests.

- Step 6b: `m5_lighting.rs`, all 29 tests of m5_lighting.robot (11 s), with leds.py's colour maths and layout reading in `tests/common`, which m4 now shares. A seeded decoder bug (RGB for GRB) fails the 13 colour tests; a radio reporting the battery 1% high fails the two that read the level.

- Step 6c: `m6_wireless.rs`, all 28 tests of m6_wireless.robot (18 s), stop mode included. Stop mode is modelled as in Renode: SysTick off, SLEEPDEEP and a WFI that any interrupt ends; what a stop does to the clocks on the chip (BOARD_FACTS.md) isn't modelled yet in either sim. A seeded UART bug (LSR never shows a byte) fails all 28. The whole suite (186 tests) takes about 100 s.

- Step 7: `m7_power.rs`, all 5 tests of m7_power.robot (3.7 s; Renode takes minutes), with the handler counts as a PC-hit counter in the chip (`Wb32::pc_hits`, Renode's `cpu AddHook`s). On the same image Renode and ours agree within a frame: lights on 5.91 M vs 6.00 M insns/s (one more 16 ms frame in our second), off 0.21 M vs 0.22 M, asleep 0.07 M both, key held 33.43 M vs 33.37 M, wakes within 1/s. Their timing drifts apart from SysTick's enable on, and it is Renode's SysTick that's off: it breaks ARM DDI 0403E B3.3 in three ways (BOARD_FACTS.md, pinned by `cpu.rs` `systick_*`), found by running the same probes from SRAM in both and logging the firmware's own CVR reads. Renode's default mode also counts instructions in translation blocks (96,555 in the first millisecond against 96,186 with `cpu MaximumBlockSize 1`, which matches ours exactly). With spec timing, one m7 press lands on a WS2812 frame and waits 9 steps for it, so the press bound is now 10 in both suites.

- Step 8: the switch-over. sim-rs is the workbench's gate; there's no test selection or smaller gate any more, as the whole suite takes about 100 s (2 min with the image builds). `run.sh` runs with `--no-fail-fast`. `sim/affected.py` is gone; Renode's `sim/run.sh` stays for the cross-check.

- Step 9: the QMK binary. `run.sh` snapshots `../qmk_5075b_plus/.build/akko_5075b_plus_via.elf` (or `$QMK_ELF`) as `qmk-via` after checking that its flash bytes are `akko_5075b_plus_fixes.bin`'s. `qmk_reference.rs` (all 8 tests of qmk_reference.robot, 7 s) and `qmk_power.rs` (compare/qmk_power.robot's 3 cases, 9 s against Renode's 25 min each) pass. The core can stub a function (`Cpu::stubs`: set r0, return), as Renode's `cpu AddHook` does with `spi_flash_start`. Two golden traces end where Renode goes wrong, not QMK: in SysTick nested over the QSPI interrupt, Renode's ICSR reads RETTOBASE, so ChibiOS's interrupt epilogue leaks a frame on the idle thread's stack per nested tick, until the stack overwrites `spiStarted` and `spi_stop` leaves the flash selected (BOARD_FACTS.md; found by watching the variable in a lockstep `compare-renode qmk-via`, pinned by `cpu.rs` `rettobase_is_clear_in_a_nested_handler`). The tests hold ours to the goldens up to that point, then to what the spec gives: CS high after a 2048-byte READ, and QMK going on to USB. QMK runs at the CPU's 100 M instructions/s, never sleeping, in both sims. The suite is 198 tests, about 2 min with the builds.

Next: a GDB stub.
