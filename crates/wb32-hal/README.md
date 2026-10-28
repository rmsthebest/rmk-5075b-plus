# wb32-hal

[Embassy](https://embassy.dev) drivers for the WestberryTech **WB32FQ95**, built on `wb32-pac`.
Each driver follows the register sequence that ChibiOS-contrib and QMK use on this chip. Those
sequences are proven on silicon, and the code was checked against the reference manual
(RM2905025).

| Module | What it does |
|---|---|
| `rcc` | Clock tree: 12 MHz HSE → PLL → 96 MHz HCLK/PCLK1/PCLK2, flash wait states; `enable_apb1` |
| `gpio` | Ports A-D: typed pins, `Output`, `Input`, `AnyPin`, every mode/pull/speed/drive/AF |
| `exti` | `ExtiInput`: async edge and level waits on EXTI lines 0-15 (`embedded-hal-async` `Wait`) |
| `time_driver` | embassy-time driver on SysTick (1 kHz interrupt, 1 MHz ticks) |
| `tick` | Periodic TIM2 interrupt, for checks that must run when the executor is stuck |
| `uart` | UART3 (C10/C11), 8N1, interrupt-driven RX ring |
| `qspi` | QSPI controller as a blocking SPI master (B3/B4/B5) |
| `spi_nor` | 25-series SPI NOR flash as `embedded-storage-async` `NorFlash`, with shared partitions |
| `ws2812` | WS2812 chain on SPIM2 MOSI (B15), interrupt-driven, 3 MHz bit encoding |
| `usb` | `embassy-usb` driver: the vendored `musb` crate plus the WB32 clock, PHY and suspend glue |
| `stop` | Stop mode entry and exit (the vendor's sequence), with clocks and USB brought back |

## Current limits (planned work before publishing)
- **Clocks:** only a 12 MHz HSE → 96 MHz is supported; `rcc::init` takes no configuration. The
  tick, UART, USB and WS2812 timings are derived from that fixed value.
- **Fixed instances and pins:** QSPI on B3-B5, UART3 only, WS2812 on SPIM2/B15. There are no
  pin traits yet.
- **Interrupts and SysTick are claimed:** the crate always defines the handlers for EXTI0-4,
  EXTI9_5, EXTI15_10, TIM2, UART3, SPIM2 and USB, and the SysTick exception. An application can't
  define them too.
- **Application choices made in the library:** `cortex-m/critical-section-single-core`,
  `embassy-time-driver/tick-hz-1_000_000`, and `wb32-pac/rt` are all switched on.
- **`spi_nor`:** tied to `Qspi` and `gpio::Output` instead of the `embedded-hal` SPI traits.
- **USB:** depends on a patched musb (`third_party/musb`). It can't be published until those
  changes are released upstream.

## Tested how
Through the firmware in this repo:
- the Renode suites in `sim/` (a model of every peripheral used here);
- QMK's binary as a reference for register sequences (`sim/golden/`);
- the board itself, for every driver.

There are no stand-alone examples yet. `firmware/src/bin/m2-blink.rs` and `m3-usb.rs` are the
closest.
