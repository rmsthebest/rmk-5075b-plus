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
| GPIO A–D | `peripherals/gpio.py` | MODER/PUPDR/ODR/BSRR/IDR, WB32 `CFGMSK` write masking, ROW2COL key matrix |
| PWR, ANCTL, IWDG, RCC | `peripherals/sysctl.py` | Ready bits follow their enables; ANCTL writes need the ANAKEY window, and locked writes are dropped and logged as `LOCKED` |

**Test control registers** (write with `sysbus WriteDoubleWord`):
- `0x4000_0FF0 = (down << 16) | (row << 8) | col` presses or releases a key.
- `0x4000_0FF4 = (level << 16) | (port << 8) | pin` forces a pin: level 0 = low, 1 = high, 2 = let go (port 0 = A). Use it for the Win/Mac switch (C14/C15), cable detect (A7) and charge-full (A15).
- `0x4000_0FF8` releases all keys.

**Not modelled yet:** add each one when its milestone needs it, and check the model against the QMK binary first.
- QSPI and the SPI NOR flash (QMK's settings): needed for M4. QMK currently stops at its first flash transfer.
- DMAC, SPIM2 (WS2812 LEDs), UART3 plus a scripted radio module: M5 and M6. The frames can come from `../tools/wls_host_test`.
- USB (MUSB): there's no model. M3 is checked on hardware.
- Timers, EXTI, ADC, the cache controller, DWT.

## Golden traces

`golden/qmk_sysctl_writes.txt` holds the ordered PWR/ANCTL/RCC writes of the flashed QMK build (`akko_5075b_plus_fixes.bin`), which is its full 96 MHz clock bring-up. The M2 clock code should produce the same sequence:

```fish
diff (sim/trace.sh target/thumbv7m-none-eabi/release/<image> 0.5 '^(PWR|ANCTL|RCC)' | psub) sim/golden/qmk_sysctl_writes.txt
```

`tests/qmk_reference.robot` re-checks the QMK build against this file, so if a model change alters the reference, that test fails.

## Limits

The simulator checks logic and register sequences. It can't prove silicon behaviour: real timing, analog behaviour, what the ROM bootloader does, or USB. The bring-up images still need one hardware check each, but only after they pass here.
