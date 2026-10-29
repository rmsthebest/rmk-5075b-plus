# Board facts

What the chip on the Akko 5075B Plus actually does, where that matters to the
simulator. The models follow these lines, not our wb32-pac or wb32-hal, and
not Renode where Renode disagrees. Each line names its source and the test
that pins it (`-` until the test is ported).

| Fact | Source | Test |
|---|---|---|
| TIM2 compare alarms fire only at the 16-bit wrap: a 1 ms alarm took 65.5 ms. Renode's `STM32_Timer` matches compares, which the chip doesn't. | Board, 2026-10-27: scan diagnostics over Vial 0xD1 (commit 9eef49c) | `tim2::tests::sixteen_bits_and_compare_at_wrap_only` |
| TIM2 is 16-bit: CNT, ARR and CCRx keep their low 16 bits. | ChibiOS `WB32_TIM2_IS_32BITS FALSE`; sim/akko.resc | `tim2::tests::sixteen_bits_and_compare_at_wrap_only` |
| Blank flash reads 0x00, not 0xFF. A DFU download leaves stale bytes past the end of a shorter image. | Board, 2026-10-26 (DFU readback) | - |
| The chip is a WB32FQ95xC: 256 KB flash, 36 KB SRAM, SYS_ID 0x3A50E980. | Board: wb32-dfu-updater_cli on download | `chip::tests` (sizes only) |
| Out of stop mode every clock is back at its reset value, USBPRE, USBCLKENR and the USB FIFO clock included. | RM2905025 5.3.4 | - |
| Stop entry is skipped while any EXTI request bit is pending. | RM2905025 5.4 | - |
| PWR_CFGR is reset only by power-on reset. | RM2905025 5.4 | - |
| SYSRESETREQ keeps SRAM: the panic record and the bootloader magic at 0x2000_6FFC survive it. | Board: boot-guard's panic record survives the reset into the ROM bootloader and back (M3, tools/m3-check.py, 2026-10-26) | - |
| The core is a Cortex-M3 r2p0 (CPUID 0x412FC230) with 4 priority bits, an MPU and 37 IRQs (0-36, I2S last). | Vendor wb32fq95xx.h (`__CM3_REV`, `__NVIC_PRIO_BITS`, `__MPU_PRESENT`, IRQn) | `cortex-m3-emu` `Config`, `ppb.rs` |
| The ROM feeds the IWDG only while RCC+0x104 is non-zero. What sets it is unknown. | ROM disassembly, rom/notes.md. Open: read 0x40010D04 on the board after starting an IWDG | - |

| AIRCR reads with bits 14:11 clear (ARMv7-M: reserved, RAZ). Renode's reads with bit 13 set, which shows in the ROM's priority-grouping write (0x05FA2300 in Renode's traces). | ARM DDI 0403E B3.2.6; unconfirmed on the board | `rom_traces.rs` |
| SysTick: RVR = N-1 gives a period of N clocks; a CVR write clears it, and RVR loads on the next clock; a disabled counter doesn't count. Renode's model differs on all three: its period is RVR clocks, a CVR write reloads at once and runs about 3 clocks ahead, and the count starts at a CVR write even before ENABLE. akko-rmk's tickless time driver restarts SysTick twice a millisecond, so Renode's wakes drift about 7 cycles/ms early, which moves its LED frames against the m7 key presses (2026-10-29: probes from SRAM and the firmware's own CVR reads, identical instructions in both). | ARM DDI 0403E B3.3; unconfirmed on the board | `cpu.rs` `systick_*` |

## Open questions for the board

Things to read with Vial peek (0xDA) in an image that's being flashed anyway:
- Reset values of the registers the models assume (RCC, ANCTL, the GPIO MODER/PUPDR).
- SYS MEMSZ (0x4001_6404); rom/sim assumes 0x03.
- RCC+0x104 with and without the IWDG running.
- Instructions per second: cycles taken by a known busy loop, against SysTick. The sim runs at Renode's 100 MIPS for now; the board's flash wait states make it slower (asm::delay(HCLK/1000) took about 3000 TIM2 ticks, 2026-10-27).
- CPUID, to confirm r2p0.
- AIRCR (0xE000_ED0C), to confirm bit 13 reads 0.
