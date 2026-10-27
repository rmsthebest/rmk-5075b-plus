#pragma once
// A fake EEPROM at small addresses: rgb_record.c does address arithmetic
// in uint32_t, which a host pointer doesn't survive.
#include <stdint.h>
#define FAKE_EEPROM_BASE 0x1000u
#define FAKE_EEPROM_SIZE 0x1000u
extern uint8_t fake_eeprom[FAKE_EEPROM_SIZE];
#define EE_AT(a) fake_eeprom[(uintptr_t)(a) - FAKE_EEPROM_BASE]
static inline uint8_t eeprom_read_byte(const uint8_t *a) { return EE_AT(a); }
static inline void eeprom_update_byte(uint8_t *a, uint8_t v) { EE_AT(a) = v; }
static inline void eeprom_write_byte(uint8_t *a, uint8_t v) { EE_AT(a) = v; }
static inline void eeprom_read_block(void *dst, const void *src, uint32_t n) {
    for (uint32_t i = 0; i < n; i++) ((uint8_t *)dst)[i] = EE_AT((uintptr_t)src + i);
}
static inline void eeprom_update_block(const void *src, void *dst, uint32_t n) {
    for (uint32_t i = 0; i < n; i++) EE_AT((uintptr_t)dst + i) = ((const uint8_t *)src)[i];
}
static inline uint32_t eeprom_read_dword(const uint32_t *a) {
    uint32_t v;
    eeprom_read_block(&v, a, 4);
    return v;
}
static inline void eeprom_update_dword(uint32_t *a, uint32_t v) { eeprom_update_block(&v, a, 4); }
