#pragma once
// The harness keeps rgb_matrix_config in RAM only.
#include <stdbool.h>
#define EECONFIG_DEBOUNCE_HELPER(name, offset, config)  \
    static inline void eeconfig_init_##name(void) {}    \
    static inline void eeconfig_flush_##name(bool f) {} \
    static inline void eeconfig_flag_##name(bool f) {}
