#include <stdio.h>
#include <assert.h>
#include "quantum.h"
#include "uart.h"
#include "module.h"
#include "smsg.h"

static uint32_t now;
uint32_t sync_timer_read32(void) { return now; }
uint32_t sync_timer_elapsed32(uint32_t t) { return now - t; }
static int resends;
void send_keyboard_report(void) { resends++; }

static uint8_t rx[512]; static int rx_head, rx_tail;
static uint8_t tx[4096]; static int tx_len;
void uart_init(uint32_t b) { (void)b; }
uint8_t uart_read(void) { return rx[rx_tail++]; }
bool uart_available(void) { return rx_tail < rx_head; }
void uart_transmit(const uint8_t *d, uint16_t n) { memcpy(tx + tx_len, d, n); tx_len += n; }

static void feed(const uint8_t *d, int n) { memcpy(rx + rx_head, d, n); rx_head += n; }
static void frame3(uint8_t a, uint8_t b) { uint8_t f[3] = {a, b, (uint8_t)(a + b)}; feed(f, 3); }
static void run(void) { for (int i = 0; i < 10; i++) md_main_task(); }
static md_stats_t *st(void) { return md_get_stats(); }

int raw_cb_len = -1;
void md_receive_raw_cb(uint8_t *p, uint8_t len) { (void)p; raw_cb_len = len; }

int main(void) {
    md_init();

    // 1. connected state frame
    frame3(MD_REV_CMD_DEVCTRL, MD_REV_CMD_DEVCTRL_CONNECTED); run();
    assert(*md_getp_state() == MD_STATE_CONNECTED);
    assert(tx_len == 3 && tx[0] == 0x61); // we ACKed it

    // 2. battery
    frame3(MD_REV_CMD_BATVOL, 57); run();
    assert(*md_getp_bat() == 57);

    // 3. lost byte: battery frame missing its checksum, then silence, then a good frame
    uint8_t partial[2] = {MD_REV_CMD_BATVOL, 11}; feed(partial, 2); run();
    now += 25; run();
    assert(st()->rx_resyncs == 1);
    frame3(MD_REV_CMD_BATVOL, 42); run();
    assert(*md_getp_bat() == 42);

    // 4. slow main loop: whole frame already in buffer long ago must NOT resync
    uint8_t first[1] = {MD_REV_CMD_BATVOL}; feed(first, 1); run();
    uint8_t rest[2] = {43, MD_REV_CMD_BATVOL + 43}; feed(rest, 2);
    now += 100; run();
    assert(*md_getp_bat() == 43 && st()->rx_resyncs == 1);

    // 5. raw frame with bogus length must not overflow, parser recovers
    uint8_t bad[3] = {MD_REV_CMD_RAW, MD_REV_CMD_RAW_OUT, 200}; feed(bad, 3); run();
    assert(st()->rx_resyncs == 2);
    frame3(MD_REV_CMD_BATVOL, 44); run();
    assert(*md_getp_bat() == 44);

    // 6. valid 32-byte raw frame
    uint8_t raw[36] = {MD_REV_CMD_RAW, MD_REV_CMD_RAW_OUT, 32};
    uint8_t sum = 0; for (int i = 0; i < 35; i++) { if (i >= 3) raw[i] = i; sum += raw[i]; } raw[35] = sum;
    feed(raw, 36); run();
    assert(raw_cb_len == 32);

    // 7. bad checksum counted, next frame fine
    uint8_t badsum[3] = {MD_REV_CMD_BATVOL, 9, 0}; feed(badsum, 3); run();
    assert(st()->checksum_errors == 1);
    frame3(MD_REV_CMD_BATVOL, 45); run(); assert(*md_getp_bat() == 45);

    // 8. disconnect counted
    frame3(MD_REV_CMD_DEVCTRL, MD_REV_CMD_DEVCTRL_DISCONNECTED); run();
    assert(st()->disconnects == 1 && *md_getp_state() == MD_STATE_DISCONNECTED);

    // 9. ACK frees the queue
    tx_len = 0;
    uint8_t kb[8] = {0, 0, 4};
    md_send_kb(kb); run();
    assert(tx_len == 10 && tx[0] == MD_SND_CMD_SEND_KB);
    uint8_t ack[3] = {0x61, 0x0D, 0x0A}; feed(ack, 3); run();
    assert(!smsg_is_busy());

    // 10. unACKed key report gets dropped after the retry limit, then a resend is requested
    tx_len = 0; resends = 0;
    md_send_kb(kb);
    for (int i = 0; i < 2000 && smsg_is_busy(); i++) { md_main_task(); now += 1; }
    assert(!smsg_is_busy());
    assert(st()->retry_drops == 1);
    md_main_task();
    assert(resends == 1);
    printf("tx_pkts=%u ack_timeouts=%u retry_drops=%u resyncs=%u cksum=%u disc=%u\n",
           st()->tx_pkts, st()->ack_timeouts, st()->retry_drops, st()->rx_resyncs, st()->checksum_errors, st()->disconnects);

    // 11. a queued packet behind a dropped one still goes out
    tx_len = 0; md_send_devctrl(MD_SND_CMD_DEVCTRL_INQVOL); md_main_task();
    assert(tx_len == 3 && tx[0] == MD_SND_CMD_DEVCTRL);
    puts("all parser/queue tests passed");
    return 0;
}
