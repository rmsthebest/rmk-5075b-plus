#pragma once
// What the board's C (5075b_plus.c, rgb_record/) needs from QMK, for the
// board harness. Pins are numbers; the EEPROM is board_harness.c's array.
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include "keycodes.h"
#include "quantum_keycodes.h"
#include "quantum_keycodes_legacy.h"
#include "action.h"
#include "action_util.h"
#include "color.h"
#include "progmem.h"
#include "timer.h"
#include "eeprom.h"
#include "eeconfig.h"
#include "rgb_matrix.h"
#include "rgblight.h"
#include "board_keycodes.h"

enum { A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15,
       B0, B1, B2, B3, B4, B5, B6, B7, B8, B9, B10, B11, B12, B13, B14, B15,
       C0, C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15 };
void gpio_write_pin_high(int pin);
void gpio_write_pin_low(int pin);
static inline void gpio_set_pin_output(int pin) {}
static inline void setPinInput(int pin) {}
static inline void setPinInputHigh(int pin) {}
static inline int readPin(int pin) { return 1; }

bool matrix_is_on(uint8_t row, uint8_t col);
bool process_record_kb(uint16_t keycode, keyrecord_t *record);
void keyboard_post_init_kb(void);
void keyboard_post_init_user(void);
bool process_record_user(uint16_t keycode, keyrecord_t *record);
bool rgb_matrix_indicators_advanced_user(uint8_t led_min, uint8_t led_max);

// Wireless: the device and the module's state are the harness's (script
// commands "devs", "wls_key", "wls_pair", "state"), and so is the battery
// ("bat"); what the module is sent and the sleep timeouts are no-ops, but
// for the manual sleep, which the harness prints.
enum { DEVS_USB = 0, DEVS_BT1, DEVS_BT2, DEVS_BT3, DEVS_BT4, DEVS_BT5, DEVS_2G4 };
enum { MD_STATE_NONE = 0, MD_STATE_PAIRING, MD_STATE_CONNECTED, MD_STATE_DISCONNECTED, MD_STATE_REJECT };
extern uint8_t harness_md_state, harness_devs, harness_bat;
void harness_sleep(void);
static inline uint8_t *md_getp_state(void) { return &harness_md_state; }
static inline uint8_t *md_getp_bat(void) { return &harness_bat; }
static inline uint8_t wireless_get_current_devs(void) { return harness_devs; }
#define MD_SND_CMD_DEVCTRL_USB 0x11
static inline void md_send_devctrl(uint8_t cmd) {}
static inline void lpwr_set_timeout_value(uint32_t t) {}
static inline void lpwr_set_timeout_manual(bool on) { if (on) harness_sleep(); }
static inline void wait_ms(uint32_t ms) {}
static inline void clear_keyboard(void) {}
static inline void send_keyboard_report(void) {}
static inline void led_wakeup(void) {}
void hs_rgb_blink_set_timer(uint32_t time);
uint32_t hs_rgb_blink_get_timer(void);
bool hs_rgb_blink_hook(void);
void wireless_devs_change_kb(uint8_t old_devs, uint8_t new_devs, bool reset);
#define USB_ACTIVE 4
extern struct usb_driver_stub { int state; } USB_DRIVER;

typedef union {
    uint16_t raw;
    struct { bool swap_control_capslock : 1; bool capslock_to_control : 1; bool swap_lalt_lgui : 1; bool swap_ralt_rgui : 1;
             bool no_gui : 1; bool swap_grave_esc : 1; bool swap_backslash_backspace : 1; bool nkro : 1; };
} keymap_config_t;
extern keymap_config_t keymap_config;
static inline uint16_t eeconfig_read_keymap(void) { return 0; }
static inline void eeconfig_update_keymap(uint16_t v) {}
