// QMK's rgb_matrix.c (and through it every effect), compiled for the host,
// driven by a script on stdin. Prints each frame the driver flushes:
//     <ms> <318 hex digits pairs: r g b per LED>
// The script (one command a line, same as the Rust side, lighting/examples/rgbref.rs):
//     cfg <enable> <mode> <h> <s> <v> <speed> <flags>
//     key <row> <col> <pressed>
//     run <from_ms> <to_ms> <step_ms>     rgb_matrix_task() at each ms in [from, to)
//     suspend <0|1>
//     op <name>                           rgb_matrix_<name>_noeeprom()
//     mode <n> | hsv <h> <s> <v> | speed <n>   the setters
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "rgb_matrix.h"

uint32_t harness_now;

// The board's RGBR_PLAY effect renders recorded colours (rgb_record.c); here
// nothing, as the Rust side's NoHooks.
void rgbrec_play(uint8_t led_min, uint8_t led_max) {}

static const struct {
    const char *name;
    void (*fn)(void);
} ops[] = {
    {"toggle", rgb_matrix_toggle_noeeprom}, {"enable", rgb_matrix_enable_noeeprom},
    {"disable", rgb_matrix_disable_noeeprom}, {"step", rgb_matrix_step_noeeprom},
    {"step_reverse", rgb_matrix_step_reverse_noeeprom}, {"increase_hue", rgb_matrix_increase_hue_noeeprom},
    {"decrease_hue", rgb_matrix_decrease_hue_noeeprom}, {"increase_sat", rgb_matrix_increase_sat_noeeprom},
    {"decrease_sat", rgb_matrix_decrease_sat_noeeprom}, {"increase_val", rgb_matrix_increase_val_noeeprom},
    {"decrease_val", rgb_matrix_decrease_val_noeeprom}, {"increase_speed", rgb_matrix_increase_speed_noeeprom},
    {"decrease_speed", rgb_matrix_decrease_speed_noeeprom},
};

// newlib's rand(), which the firmware links (glibc's differs).
static uint64_t rand_next = 1;
int rand(void) {
    rand_next = rand_next * 6364136223846793005ULL + 1;
    return (int)((rand_next >> 32) & RAND_MAX);
}

// rgb_matrix_drivers.c's WS2812 driver, plus a bounds check: effects that
// touch a matrix position without an LED pass NO_LED (255), which on the
// board writes past the array.
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

int main(void) {
    char line[256];
    while (fgets(line, sizeof line, stdin)) {
        unsigned a[7];
        if (sscanf(line, "cfg %u %u %u %u %u %u %u", &a[0], &a[1], &a[2], &a[3], &a[4], &a[5], &a[6]) == 7) {
            rgb_matrix_config.enable = a[0];
            rgb_matrix_config.mode   = a[1];
            rgb_matrix_config.hsv.h  = a[2];
            rgb_matrix_config.hsv.s  = a[3];
            rgb_matrix_config.hsv.v  = a[4];
            rgb_matrix_config.speed  = a[5];
            rgb_matrix_config.flags  = a[6];
        } else if (sscanf(line, "key %u %u %u", &a[0], &a[1], &a[2]) == 3) {
            rgb_matrix_handle_key_event(a[0], a[1], a[2]);
        } else if (sscanf(line, "run %u %u %u", &a[0], &a[1], &a[2]) == 3) {
            for (uint32_t t = a[0]; t < a[1]; t += a[2]) {
                harness_now = t;
                rgb_matrix_task();
            }
        } else if (sscanf(line, "mode %u", &a[0]) == 1) {
            rgb_matrix_mode_noeeprom(a[0]);
        } else if (sscanf(line, "hsv %u %u %u", &a[0], &a[1], &a[2]) == 3) {
            rgb_matrix_sethsv_noeeprom(a[0], a[1], a[2]);
        } else if (sscanf(line, "speed %u", &a[0]) == 1) {
            rgb_matrix_set_speed_noeeprom(a[0]);
        } else if (strncmp(line, "op ", 3) == 0) {
            char name[32];
            sscanf(line + 3, "%31s", name);
            size_t i = 0;
            while (i < sizeof ops / sizeof ops[0] && strcmp(ops[i].name, name)) i++;
            if (i == sizeof ops / sizeof ops[0]) {
                fprintf(stderr, "bad op: %s", line);
                return 1;
            }
            ops[i].fn();
        } else if (sscanf(line, "suspend %u", &a[0]) == 1) {
            rgb_matrix_set_suspend_state(a[0]);
        } else if (line[0] != '\n' && line[0] != '#') {
            fprintf(stderr, "bad line: %s", line);
            return 1;
        }
    }
    return 0;
}
