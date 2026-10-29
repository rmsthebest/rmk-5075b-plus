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
    uint8_t d[64]; for (int i = 0; i < 64; i++) d[i] = i * 7 + 1;
    md_send_kb(d); flush(); md_send_nkro(d); flush(); md_send_consumer(d); flush(); md_send_system(d); flush();
    md_send_fn(d); flush(); md_send_mouse(d); flush(); md_send_devctrl(0x53); flush();
    md_send_rf_test(0x13, 5, 1); flush(); md_send_rf_test_stop(); flush();
    md_send_manufacturer((char *)d, 10); flush(); md_send_product((char *)d, 46); flush(); md_send_vpid(0x342D, 0xE4F6); flush();
    md_send_raw(d, 32); flush();
    for (int dev = 0; dev <= 6; dev++) { md_devs_change(dev, false); flush(); md_devs_change(dev, true); flush(); }
    return 0;
}
