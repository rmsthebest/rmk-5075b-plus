/* The chip is a WB32FQ95xC (the DFU tool reports 256 KB flash, 36 KB SRAM),
   although QMK links it as an xB (128K/28K). */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 256K
  /* 28K minus the last 8 bytes: 0x20006FFC holds the bootloader magic word
     (boot-guard), shared with QMK. Keep it out of .data/.bss/stack. The SRAM
     above it holds boot-guard's panic record (0x20007000). */
  RAM   : ORIGIN = 0x20000000, LENGTH = 28K - 8
}
