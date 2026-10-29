#pragma once
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#define PRODUCT "Test"
#define MANUFACTURER "Test"
#define VENDOR_ID 0x342D
#define PRODUCT_ID 0xE4F6
uint32_t sync_timer_read32(void);
uint32_t sync_timer_elapsed32(uint32_t t);
void send_keyboard_report(void);
