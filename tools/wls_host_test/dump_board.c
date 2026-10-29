// The board's device-select frames, with the board's strings and ARM's -fshort-wchar.
#include <stdio.h>
#include "quantum.h"
#include "uart.h"
#include "module.h"
#include "smsg.h"
uint32_t sync_timer_read32(void) { return 0; }
uint32_t sync_timer_elapsed32(uint32_t t) { (void)t; return 0; }
void send_keyboard_report(void) {}
static const uint8_t ack[3] = {0x61, 0x0D, 0x0A}; static int ai = 3;
void uart_init(uint32_t b) { (void)b; }
uint8_t uart_read(void) { return ack[ai++]; }
bool uart_available(void) { return ai < 3; }
void uart_transmit(const uint8_t *d, uint16_t n) { for (int i = 0; i < n; i++) printf("%02x", d[i]); printf("\n"); }
static void flush(void) { while (smsg_is_busy()) { md_main_task(); ai = 0; md_main_task(); } }
int main(void) {
    md_init();
    for (int dev = 0; dev <= 6; dev++) { md_devs_change(dev, false); flush(); md_devs_change(dev, true); flush(); }
    return 0;
}
