/* WB32FQ95xB, as QMK links it (MCU_LDSCRIPT = WB32FQ95xB). */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 128K
  /* 28K minus the last 8 bytes: 0x20006FFC holds the bootloader magic word
     (boot-guard), shared with QMK. Keep it out of .data/.bss/stack. */
  RAM   : ORIGIN = 0x20000000, LENGTH = 28K - 8
}
