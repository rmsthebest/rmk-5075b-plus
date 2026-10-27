// The board's own lighting C (5075b_plus.c's key and indicator code, which
// tools/rgbref/board.py lifts out, and rgb_record/) on top of QMK's
// rgb_matrix.c, rgblight.c and process_rgb.c, compiled for the host and
// driven by a script on stdin. Prints, as the Rust side
// (lighting/examples/boardref.rs) does:
//     <ms> <hex r g b per LED>        each frame the driver flushes
//     P <ms> <0|1>                    the LED power pin (A8) changing
//     D ...                           the state, for "dump"
// The script, one command a line:
//     boot <lctl> <esc>               power-up with an erased EEPROM, keys held
//     key <name> <row> <col> <pressed> <shift>
//     run <from_ms> <to_ms> <step_ms> rgblight_task + rgb_matrix_task each step
//     usb <0|1>                       USB configured or not
//     dump
// "defaults" alone prints the default recordings (keymap.c) for the Rust side.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "quantum.h"
#include "process_rgb.h"

uint32_t harness_now;
uint8_t  fake_eeprom[FAKE_EEPROM_SIZE];
static uint32_t fake_kb;
struct usb_driver_stub USB_DRIVER = {USB_ACTIVE};
keymap_config_t keymap_config;
static uint8_t mods;
static bool    held_lctl, held_esc;
static int     power = -1;

extern const uint16_t rgbrec_default_effects[RGBREC_CHANNEL_NUM][MATRIX_ROWS][MATRIX_COLS];
extern bool led_status;
extern rgblight_config_t rgblight_config;
typedef union {
    uint32_t raw;
    struct {
        uint8_t flag : 1;
        uint8_t devs : 3;
        uint8_t record_channel : 4;
        uint8_t record_last_mode;
        uint8_t last_btdevs : 3;
        uint8_t dir_flag : 1;
        uint8_t ctrl_app_flag : 1;
        uint8_t rgb_enable : 1;
        uint8_t sleep_timeout : 3;
    };
} confinfo_view_t;
extern confinfo_view_t confinfo;

uint32_t eeconfig_read_kb(void) { return fake_kb; }
void     eeconfig_update_kb(uint32_t v) { fake_kb = v; }
// eeconfig_init: the wear-leveling EEPROM is formatted (all 0 here), then
// the board's datablock defaults go in.
void eeconfig_init(void) {
    memset(fake_eeprom, 0, sizeof fake_eeprom);
    fake_kb = 0;
    extern void eeconfig_init_user_datablock(void);
    eeconfig_init_user_datablock();
}

void gpio_write_pin_high(int pin) { if (pin == LED_POWER_EN_PIN) power = 1; }
void gpio_write_pin_low(int pin) { if (pin == LED_POWER_EN_PIN) power = 0; }
bool matrix_is_on(uint8_t row, uint8_t col) { return (row == 5 && col == 0 && held_lctl) || (row == 0 && col == 0 && held_esc); }
void keyboard_post_init_user(void) {}
bool process_record_user(uint16_t keycode, keyrecord_t *record) { return true; }
uint8_t get_mods(void) { return mods; }

// rgb_matrix_drivers.c's WS2812 driver (as harness.c).
static rgb_led_t leds[RGB_MATRIX_LED_COUNT];
static bool      dirty;
static void drv_init(void) {}
static void drv_flush(void) {
    if (!dirty) return;
    dirty = false;
    printf("%u ", harness_now);
    for (int i = 0; i < RGB_MATRIX_LED_COUNT; i++) printf("%02x%02x%02x", leds[i].r, leds[i].g, leds[i].b);
    printf("\n");
}
static void drv_set(int i, uint8_t r, uint8_t g, uint8_t b) {
    if (i < 0 || i >= RGB_MATRIX_LED_COUNT) return;
    if (leds[i].r == r && leds[i].g == g && leds[i].b == b) return;
    dirty     = true;
    leds[i].r = r;
    leds[i].g = g;
    leds[i].b = b;
}
static void drv_set_all(uint8_t r, uint8_t g, uint8_t b) {
    for (int i = 0; i < RGB_MATRIX_LED_COUNT; i++) drv_set(i, r, g, b);
}
const rgb_matrix_driver_t rgb_matrix_driver = {
    .init = drv_init, .flush = drv_flush, .set_color = drv_set, .set_color_all = drv_set_all,
};

// newlib's rand(), which the firmware links.
static uint64_t rand_next = 1;
int rand(void) {
    rand_next = rand_next * 6364136223846793005ULL + 1;
    return (int)((rand_next >> 32) & RAND_MAX);
}

static const struct {
    const char *name;
    uint16_t    kc;
} keys[] = {
    {"RGB_TOG", RGB_TOG}, {"RGB_MOD", RGB_MOD}, {"RGB_RMOD", RGB_RMOD}, {"RGB_HUI", RGB_HUI}, {"RGB_HUD", RGB_HUD},
    {"RGB_SAI", RGB_SAI}, {"RGB_SAD", RGB_SAD}, {"RGB_VAI", RGB_VAI}, {"RGB_VAD", RGB_VAD}, {"RGB_SPI", RGB_SPI},
    {"RGB_SPD", RGB_SPD}, {"RL_MOD", RL_MOD}, {"RP_P0", RP_P0}, {"RP_P1", RP_P1}, {"RP_P2", RP_P2},
    {"RP_END", RP_END}, {"NK_TOGG", NK_TOGG}, {"EE_CLR", EE_CLR}, {"KC_ENCW", KC_ENCW}, {"MO_FL", MO(1)},
    {"MO_FBL", MO(4)}, {"KC_A", KC_A},
};

static void power_check(void) {
    static int last = -1;
    if (power != last) {
        printf("P %u %d\n", harness_now, power);
        last = power;
    }
}

static void dump(void) {
    printf("D m %u %u %u %u %u %u %u", rgb_matrix_config.enable, rgb_matrix_config.mode, rgb_matrix_config.hsv.h,
           rgb_matrix_config.hsv.s, rgb_matrix_config.hsv.v, rgb_matrix_config.speed, rgb_matrix_config.flags);
    printf(" l %u %u %u %u %u %u", rgblight_config.enable, rgblight_config.mode, rgblight_config.hue, rgblight_config.sat,
           rgblight_config.val, rgblight_config.speed);
    printf(" c %u %u %u %u", confinfo.record_channel, confinfo.record_last_mode, confinfo.rgb_enable, led_status);
    printf(" i");
    const uint8_t *colours = (const uint8_t *)(uintptr_t)((uint32_t)CONFINFO_EECONFIG_ADDR + 4);
    for (int i = 0; i < 16; i++) printf(" %u", eeprom_read_byte(colours + i));
    printf(" r");
    for (int i = 0; i < RGBREC_CHANNEL_NUM * MATRIX_ROWS * MATRIX_COLS; i++) {
        const uint8_t *p = RGBREC_EECONFIG_ADDR + i * 2;
        printf(" %u", eeprom_read_byte(p) | eeprom_read_byte(p + 1) << 8);
    }
    printf("\n");
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "defaults")) {
        for (int c = 0; c < RGBREC_CHANNEL_NUM; c++)
            for (int r = 0; r < MATRIX_ROWS; r++)
                for (int k = 0; k < MATRIX_COLS; k++) printf("%u ", rgbrec_default_effects[c][r][k]);
        printf("\n");
        return 0;
    }
    char line[256];
    while (fgets(line, sizeof line, stdin)) {
        unsigned a[5];
        char     name[32];
        if (sscanf(line, "boot %u %u", &a[0], &a[1]) == 2) {
            held_lctl = a[0];
            held_esc  = a[1];
            // keyboard_init with an erased EEPROM: eeconfig_init, then the
            // inits, then keyboard_post_init_kb.
            eeconfig_init();
            rgblight_init();
            rgb_matrix_init();
            keyboard_post_init_kb();
            power_check();
        } else if (sscanf(line, "key %31s %u %u %u %u", name, &a[0], &a[1], &a[2], &a[3]) == 5) {
            size_t i = 0;
            while (i < sizeof keys / sizeof keys[0] && strcmp(keys[i].name, name)) i++;
            if (i == sizeof keys / sizeof keys[0]) {
                fprintf(stderr, "bad key: %s", line);
                return 1;
            }
            mods          = a[3] ? 0x02 : 0; // left Shift
            keyrecord_t r = {.event = {.key = {.row = a[0], .col = a[1]}, .pressed = a[2], .time = harness_now}};
            rgb_matrix_handle_key_event(a[0], a[1], a[2]);
            if (process_record_kb(keys[i].kc, &r)) process_rgb(keys[i].kc, &r);
            power_check();
        } else if (sscanf(line, "run %u %u %u", &a[0], &a[1], &a[2]) == 3) {
            for (uint32_t t = a[0]; t < a[1]; t += a[2]) {
                harness_now = t;
                rgblight_task();
                rgb_matrix_task();
                power_check();
            }
        } else if (sscanf(line, "usb %u", &a[0]) == 1) {
            USB_DRIVER.state = a[0] ? USB_ACTIVE : 0;
        } else if (!strncmp(line, "dump", 4)) {
            dump();
        } else if (line[0] != '\n' && line[0] != '#') {
            fprintf(stderr, "bad line: %s", line);
            return 1;
        }
    }
    return 0;
}
