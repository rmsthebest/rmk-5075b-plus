# Simulated Akko 5075B Plus (Renode)

Since 2026-10-29 the gate for images is our own simulator, `../sim-rs` (`sim-rs/run.sh`,
about 2 min). This Renode sim runs the same suites as a cross-check (the workbench's
"Cross-check in Renode"), and the QMK binary runs in both (sim-rs's `qmk_reference.rs` and
`qmk_power.rs`). Where the two sims differ, sim-rs follows the chip
and the ARM spec; `../sim-rs/BOARD_FACTS.md` lists where Renode's models don't (its SysTick,
and ICSR.RETTOBASE, which breaks QMK's SPI flash here: the QMK golden traces end there).
Since sim-rs times the core from the chip's clock tree (96 MHz for our images, not a flat
100 MIPS), its m7_power and qmk_power keep their own bounds; the robot files keep Renode's.

Runs our images, and the known-good QMK build, on a simulated WB32FQ95, so most
changes can be checked without flashing the keyboard.

```fish
sim/run.sh                                  # build images, run every suite
sim/run.sh sim/tests/boot_guard.robot       # one suite
SIM_MATCH='vial|knob' sim/run.sh            # only tests whose name matches (regex)
SIM_JOBS=4 sim/run.sh                       # Renode instances at once (default: half the CPUs, max 16)
sim/trace.sh <image.elf|.bin> [secs] [re]   # ordered register writes, for diffing
```

Each test runs as its own suite (`sim/shard.py` writes them to `target/sim-shards`), and
`renode-test -j` runs them in parallel; the run takes about as long as its slowest test.

Reports: `target/sim-results/report.html`, with a per-test Renode log in `target/sim-results/logs/`.

## Setup (once)

Renode is a portable download, so no system install is needed:

From the repo root (`run.sh` looks in `../tools`, next to the repo):

```fish
mkdir -p ../tools; cd ../tools
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
| SysTick | Renode's built-in (NVIC `systickFrequency` 96 MHz) | The time base, as QMK's ChibiOS tick. The earlier TIM2 driver passed here, but on the board its compare alarms only fired at the 16-bit wrap (one 1 ms timer took 65.5 ms): Renode's `STM32_Timer` matches compares the chip doesn't |
| QSPI + SPI NOR flash | `peripherals/qspi.py` | The DesignWare SSI controller in SPI mode (CR0/CR1/BAUDR locked while enabled, RX FIFO with overflow, TXE/RXF/RXO interrupts on NVIC 26) and a 512 KB 25-series NOR on it, chip select on C12. WREN/WIP/page-wrap rules are enforced and broken ones logged as errors; every erase and program is logged (`flash: SE 0x010000`), and the contents survive a machine reset. The JEDEC ID is QMK's default part, since the real one is unknown |
| Flash cache | `peripherals/cache.py` | Remembers writes, so the clock setup's cache write shows in traces |
| DWT | Renode `Miscellaneous.DWT` (in the `.repl`) | CYCCNT at 96 MHz; QMK's ChibiOS busy-waits on it |
| USB (MUSB) + a USB host | `peripherals/usb.py` | Device registers, FIFOs and interrupts, plus a scripted Linux-like host that attaches once PA11/PA12, the USB clocks (48 MHz) and the PHY are set up, enumerates, then polls every IN endpoint. Logs `HOST: ...` per request, `HOST CDC: <line>` for CDC-ACM text, `HOST HID <ep>: <hex>` for reports, and `HOST ERROR: ...` for anything a host wouldn't accept. The register semantics it assumes are listed at the top of the file. Also covers CRC and SFM (the PHY enable) as plain registers |

**Test control registers** (write with `sysbus WriteDoubleWord`; the USB host's with `usb WriteDoubleWord`):
- `0x4000_0FF0 = (down << 16) | (row << 8) | col` presses or releases a key.
- `0x4000_0FF4 = (level << 16) | (port << 8) | pin` forces a pin: level 0 = low, 1 = high, 2 = let go (port 0 = A). Use it for the Win/Mac switch (C14/C15), cable detect (A7) and charge-full (A15).
- `0x4000_0FF8` releases all keys.
- Open-drain outputs set high don't drive the pin; it reads as its pull (the RMK scan drives its columns open-drain).
- SPI flash, `qspi` offset `0xF0`: address for `0xF4`; `0xF4` reads or programs the word there (no log, address advances); `0xF8` erases the whole chip.
- USB host, `usb` offset `0xF00`: `0x01` open the CDC port, `0x02 | byte << 8` send a byte to it, `0x03 | leds << 8` keyboard LED report, `0x04`/`0x05` suspend/resume, `0x06` reset and enumerate again, `0x07`/`0x08` unplug/plug, `0x09 | ep << 8` send the bytes queued by writes to `0xF0C` as one packet on interrupt OUT endpoint `ep` (raw HID, e.g. Vial). `0x0A | wIndex << 8` sends CLEAR_FEATURE(ENDPOINT_HALT) for that endpoint (any number, including ones the device doesn't have); `0x0B` starts a GET_DESCRIPTOR, abandons it after the SETUP (SetupEnd), then reads the device descriptor and fails the host if that doesn't come back. `0xF04` reads the host state (0 detached, 1 enumerating, 2 configured, 3 suspended, 4 failed). Writing 1 to `0xF10` (after `machine Reset`, which re-initialises the model) logs every USB register access as `USBT R|W i<index> +0x<off> <-|-> 0x<value>`, for comparing a driver's register sequence with QMK's.

**Not modelled yet:** add each one when its milestone needs it, and check the model against the QMK binary first.
- DMAC, SPIM2 (WS2812 LEDs), UART3 plus a scripted radio module: M5 and M6. The frames can come from `../tools/wls_host_test`.
- TIM1/3/4, ADC, and the cache's actual behaviour.

**QMK hooks.** On the blank simulated flash, QMK reads its settings (that part is checked against `golden/qmk_qspi_init.txt`), then HardFaults (INVSTATE) in a context switch during `init_usb_driver`'s first sleep. That's not investigated. It might be a model problem or QMK's fresh-settings path, and the real chip isn't blank. `qmk-hooks.resc` makes QMK's `spi_flash_start()` fail at once, so QMK skips the flash and reaches USB. Include it after loading the QMK image (`SIM_SETUP='include @sim/qmk-hooks.resc' sim/trace.sh ...` for traces). QMK then runs with unreadable settings: it enumerates, but sends its key reports to the radio module, so its HID IN reports can't be checked yet.

## Golden traces

`golden/qmk_sysctl_writes.txt` holds the ordered PWR/ANCTL/RCC writes of the flashed QMK build (`akko_5075b_plus_fixes.bin`): its 96 MHz clock bring-up, then the board's peripheral clock enables. To compare an image by hand:

```fish
diff (sim/trace.sh target/thumbv7m-none-eabi/release/m2-blink 0.05 '^(PWR|ANCTL|RCC|CACHE)' | tail -n +7 | psub) sim/golden/qmk_clock_init.txt
```

`golden/qmk_clock_init.txt` is the part of that sequence `wb32_hal::rcc::init` ports (through the APB2 bus-matrix enable), with the cache write included. `tests/m2_hal.robot` checks that m2-blink produces exactly this after boot-guard's own 6 writes.

`golden/qmk_qspi_init.txt` is QMK's QSPI setup, chip select and first flash commands on a blank chip (QSPI writes, GPIOC BSRR and the model's `FLASH` lines).

`golden/qmk_usb_init.txt` is QMK's USB bring-up (ChibiOS `wb32_usb_init`: USB and FIFO clocks, reset, PHY, 48 MHz prescaler), which `wb32_hal::usb` reproduces write for write.

`tests/qmk_reference.robot` re-checks the QMK build against these files, so if a model change alters the reference, that test fails.

## Suites

| Suite | Image | Checks |
|---|---|---|
| `boot_guard.robot` | m1a-bounce, m1b-stub | DFU entry by magic word, Esc at cold start, HardFault; pin state restored |
| `qmk_reference.robot` | QMK fixes build | The models against known-good firmware, including enumeration and the LED report on the USB model |
| `../compare/qmk_power.robot` | vendor QMK (an ELF, `--variable QMK:`) | Not in `run.sh`: m7_power's CPU numbers for the vendor build, for the README's comparison (about 25 min per case) |
| `m3_usb.robot` | m3-usb | USB bring-up trace = QMK's; attach after 50 ms; enumeration as CDC-ACM + HID (IN/OUT sharing endpoint numbers); boot banner and commands over CDC; `type` sends the right reports; host Caps Lock drives the LED; suspend/resume in order and after every bus reset; `dfu`, Esc and panic all reach DFU; the panic message is printed once after the next boot; DFU after 30 s without a host |
| `m4_rmk.robot` | akko-rmk | The RMK keyboard: enumerates with QMK's VID/PID as keyboard + composite + Vial; keys, the Fn layer's consumer keys, the knob both ways (and three quick detents, none lost), Win/Mac switch swapping Gui/Alt, host Caps Lock on the LED; Fn+RShift+Esc and Esc at a cold start reach DFU; storage only in SPI flash 0x10000-0x1FFFF (markers in QMK's block untouched); a Vial keymap change persists across a reset; the stack keeps at least 4 KB spare |
| `m2_hal.robot` | m2-blink | Clock trace = QMK's; SysTick set up for 1 ms at 96 MHz; the time base counts milliseconds; 500 ms blink edges within ±10 ms and DFU at 10 s; Esc via EXTI returns to DFU within 2 ms |

## Limits

The simulator checks logic and register sequences. It can't prove silicon behaviour: real timing, analog behaviour, or what the ROM bootloader does (for example, whether it overwrites the panic record at `0x2000_7000`). The USB model is written from the Mentor programming model and checked against QMK's ChibiOS driver, but where the two drivers depend on details neither exercises (the exact status-stage handshake, when FADDR takes effect, what a bus reset clears), it encodes assumptions; `tools/m3-check.py` is the hardware check for those. The M4 suite turns the GPIO log off (`logLevel 3 gpio`), as the scan's pin traffic slows the simulation; and its waits on `HOST:` lines in the middle of enumeration don't pause the emulation, which trips the USB model (an index error on the next tick). The bring-up images still need one hardware check each, but only after they pass here.
