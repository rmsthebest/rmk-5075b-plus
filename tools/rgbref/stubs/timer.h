#pragma once
#include <stdint.h>
// The harness's clock, set before each rgb_matrix_task().
extern uint32_t harness_now;
static inline uint16_t timer_read(void) { return (uint16_t)harness_now; }
static inline uint32_t timer_read32(void) { return harness_now; }
static inline uint16_t timer_elapsed(uint16_t t) { return (uint16_t)((uint16_t)harness_now - t); }
static inline uint32_t timer_elapsed32(uint32_t t) { return harness_now - t; }
