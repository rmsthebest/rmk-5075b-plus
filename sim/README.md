# Simulated Akko 5075B Plus (Renode)

Runs our images, and the known-good QMK build, on a simulated WB32FQ95, so most
changes can be checked without flashing the keyboard.

```fish
sim/run.sh                                  # build images, run every suite
sim/run.sh sim/tests/boot_guard.robot       # one suite
sim/trace.sh <image.elf|.bin> [secs] [re]   # ordered register writes, for diffing
```

Reports: `target/sim-results/report.html`, with a per-test Renode log in `target/sim-results/logs/`.

## Setup (once)

Renode is a portable download, so no system install is needed:

```fish
cd ~/stuff/akko/tools
mkdir renode-dl renode
curl -sSL -o renode-dl/renode.tar.gz https://github.com/renode/renode/releases/download/v1.17.0/renode-1.17.0.linux-portable.tar.gz
# sha256 4ba7c68b59e2447f188ef4b4b112fcccd2802582c460beedceb63b84e5605a5f
tar -xzf renode-dl/renode.tar.gz -C renode --strip-components=1
python3 -m venv renode-venv
renode-venv/bin/pip install -r renode/tests/requirements.txt
```

`run.sh` expects them at `../tools/renode` and `../tools/renode-venv`. Override with `RENODE=` and `RENODE_VENV=`.

## What's modelled

| Block | Model | Behaviour |
|---|---|---|
| Cortex-M3, NVIC, SysTick | Renode built-in | Interrupts, `SYSRESETREQ` resets with RAM kept (as on the chip) |
| Flash `0x0800_0000` (+ alias at 0), SRAM 36K | memory | |
| Boot ROM `0x1FFF_E000` | stub in `akko.resc` | Parks at `0x1FFF_E008`, which means "entered DFU" |
| GPIO A–D, AFIO, EXTI | `peripherals/gpio.py` | MODER/PUPDR/ODR/BSRR/IDR, WB32 `CFGMSK` write masking, ROW2COL key matrix. EXTI lines 0–15 follow the pin AFIO selects; enabled edges set PR and drive the EXTI0–4/9_5/15_10 NVIC lines |
| PWR, ANCTL, IWDG, RCC | `peripherals/sysctl.py` | Ready bits follow their enables; ANCTL writes need the ANAKEY window, and locked writes are dropped and logged as `LOCKED` |
| TIM2 | Renode `STM32_Timer` (in the `.repl`) | Same register layout as WB32's TIM; 32-bit, 96 MHz input. QMK doesn't use it (its tick is SysTick), so `m2_hal.robot` checks it against virtual time |
| Flash cache | `peripherals/cache.py` | Remembers writes, so the clock setup's cache write shows in traces |

**Test control registers** (write with `sysbus WriteDoubleWord`):
- `0x4000_0FF0 = (down << 16) | (row << 8) | col` presses or releases a key.
- `0x4000_0FF4 = (level << 16) | (port << 8) | pin` forces a pin: level 0 = low, 1 = high, 2 = let go (port 0 = A). Use it for the Win/Mac switch (C14/C15), cable detect (A7) and charge-full (A15).
- `0x4000_0FF8` releases all keys.

**Not modelled yet:** add each one when its milestone needs it, and check the model against the QMK binary first.
- QSPI and the SPI NOR flash (QMK's settings): needed for M4. QMK currently stops at its first flash transfer.
- DMAC, SPIM2 (WS2812 LEDs), UART3 plus a scripted radio module: M5 and M6. The frames can come from `../tools/wls_host_test`.
- USB (MUSB): there's no model. M3 is checked on hardware.
- TIM1/3/4, ADC, DWT, and the cache's actual behaviour.

## Golden traces

`golden/qmk_sysctl_writes.txt` holds the ordered PWR/ANCTL/RCC writes of the flashed QMK build (`akko_5075b_plus_fixes.bin`): its 96 MHz clock bring-up, then the board's peripheral clock enables. To compare an image by hand:

```fish
diff (sim/trace.sh target/thumbv7m-none-eabi/release/m2-blink 0.05 '^(PWR|ANCTL|RCC|CACHE)' | tail -n +7 | psub) sim/golden/qmk_clock_init.txt
```

`golden/qmk_clock_init.txt` is the part of that sequence `wb32_hal::rcc::init` ports (through the APB2 bus-matrix enable), with the cache write included. `tests/m2_hal.robot` checks that m2-blink produces exactly this after boot-guard's own 6 writes.

`tests/qmk_reference.robot` re-checks the QMK build against both files, so if a model change alters the reference, that test fails.

## Suites

| Suite | Image | Checks |
|---|---|---|
| `boot_guard.robot` | m1a-bounce, m1b-stub | DFU entry by magic word, Esc at cold start, HardFault; pin state restored |
| `qmk_reference.robot` | QMK fixes build | The models against known-good firmware |
| `m2_hal.robot` | m2-blink (3 builds) | Clock trace = QMK's; 1 MHz time base; 500 ms blink edges within ±10 ms and DFU at 10 s; the time driver's period count across the half-wrap and wrap (`sim-time-near-half`/`-wrap` builds start 2 s before them); Esc via EXTI returns to DFU within 2 ms |

## Limits

The simulator checks logic and register sequences. It can't prove silicon behaviour: real timing, analog behaviour, what the ROM bootloader does, or USB. The bring-up images still need one hardware check each, but only after they pass here.
