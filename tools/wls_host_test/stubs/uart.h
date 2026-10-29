#pragma once
#include <stdint.h>
#include <stdbool.h>
void uart_init(uint32_t baud);
uint8_t uart_read(void);
void uart_transmit(const uint8_t *data, uint16_t length);
bool uart_available(void);
