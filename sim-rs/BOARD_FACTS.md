# Board facts

What the chip on the Akko 5075B Plus actually does, where that matters to the
simulator. The models follow these lines, not our wb32-pac or wb32-hal, and
not Renode where Renode disagrees. Each line names its source and the test
that pins it (`-` until the test is ported).

| Fact | Source | Test |
|---|---|---|
| TIM2 compare alarms fire only at the 16-bit wrap: a 1 ms alarm took 65.5 ms. Renode's `STM32_Timer` matches compares, which the chip doesn't. | Board, 2026-10-27: scan diagnostics over Vial 0xD1 (commit 9eef49c) | - |
| TIM2 is 16-bit: CNT, ARR and CCRx keep their low 16 bits. | ChibiOS `WB32_TIM2_IS_32BITS FALSE`; sim/akko.resc | - |
| Blank flash reads 0x00, not 0xFF. A DFU download leaves stale bytes past the end of a shorter image. | Board, 2026-10-26 (DFU readback) | - |
| The chip is a WB32FQ95xC: 256 KB flash, 36 KB SRAM, SYS_ID 0x3A50E980. | Board: wb32-dfu-updater_cli on download | `chip::tests` (sizes only) |
| Out of stop mode every clock is back at its reset value, USBPRE, USBCLKENR and the USB FIFO clock included. | RM2905025 5.3.4 | - |
| Stop entry is skipped while any EXTI request bit is pending. | RM2905025 5.4 | - |
| PWR_CFGR is reset only by power-on reset. | RM2905025 5.4 | - |
| SYSRESETREQ keeps SRAM: the panic record and the bootloader magic at 0x2000_6FFC survive it. | Board: boot-guard's panic record survives the reset into the ROM bootloader and back (M3, tools/m3-check.py, 2026-10-26) | - |
| The ROM feeds the IWDG only while RCC+0x104 is non-zero. What sets it is unknown. | ROM disassembly, rom/notes.md. Open: read 0x40010D04 on the board after starting an IWDG | - |

## Open questions for the board

Things to read with Vial peek (0xDA) in an image that's being flashed anyway:
- Reset values of the registers the models assume (RCC, ANCTL, the GPIO MODER/PUPDR).
- SYS MEMSZ (0x4001_6404); rom/sim assumes 0x03.
- RCC+0x104 with and without the IWDG running.
- Instructions per second: cycles taken by a known busy loop, against SysTick.
