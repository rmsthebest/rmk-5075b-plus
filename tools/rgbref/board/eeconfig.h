#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "eeprom.h"
// rgb_matrix's config lives in RAM; the board's user datablock in the fake
// EEPROM; the kb word (confinfo) in a variable.
#define EECONFIG_DEBOUNCE_HELPER(name, offset, config)  \
    static inline void eeconfig_init_##name(void) {}    \
    static inline void eeconfig_flush_##name(bool f) {} \
    static inline void eeconfig_flag_##name(bool f) {}
// rgblight's config, after the datablock.
#define EECONFIG_RGBLIGHT ((uint32_t *)(uintptr_t)(FAKE_EEPROM_BASE + 0xF00))
#define EECONFIG_RGBLIGHT_EXTENDED ((uint8_t *)(uintptr_t)(FAKE_EEPROM_BASE + 0xF04))
#define EECONFIG_USER_DATABLOCK ((uint8_t *)(uintptr_t)FAKE_EEPROM_BASE)
uint32_t eeconfig_read_kb(void);
void eeconfig_update_kb(uint32_t v);
void eeconfig_init(void);
