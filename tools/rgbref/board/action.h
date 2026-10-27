#pragma once
#include <stdint.h>
#include <stdbool.h>
#include "quantum_keycodes.h"
typedef struct { uint8_t col; uint8_t row; } keypos_t;
typedef struct { keypos_t key; bool pressed; uint16_t time; } keyevent_t;
typedef struct { keyevent_t event; } keyrecord_t;
