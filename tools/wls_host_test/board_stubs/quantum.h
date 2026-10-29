#pragma once
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#define PRODUCT "AKKO Keyboard"
#define MANUFACTURER "AKKO"
#define VENDOR_ID 0x342D
#define PRODUCT_ID 0xE4F6
uint32_t sync_timer_read32(void);
uint32_t sync_timer_elapsed32(uint32_t t);
void send_keyboard_report(void);
#define MD_BT1_NAME "Akko 5075-1"
#define MD_BT2_NAME "Akko 5075-2"
#define MD_BT3_NAME "Akko 5075-3"
#define MD_DONGLE_PRODUCT "Akko 2.4G VIA KB"
