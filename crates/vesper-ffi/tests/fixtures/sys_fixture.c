#include <stddef.h>
#include <stdint.h>

uint32_t bridge_sum(uint32_t a, uint32_t b) { return a + b; }
int16_t bridge_negative(void) { return -17; }
/* Logical bool/enum refinements use total-bit carriers, NOT C _Bool/enum. */
uint8_t bridge_bool(void) { return 2; }
uint32_t bridge_state(void) { return 9; }
uint32_t *bridge_address(void) { static uint32_t cell = 42; return &cell; }
size_t bridge_copy(const uint8_t *src, size_t src_len,
                   uint8_t *dst, size_t dst_len) {
    size_t count = src_len < dst_len ? src_len : dst_len;
    for (size_t i = 0; i < count; ++i) dst[i] = src[i] ^ 0x5a;
    return count;
}
/* Single-element nocapture pointers: one scalar inout, one struct out. */
void bridge_crc_update(uint32_t *crc, const uint8_t *data, size_t len) {
    uint32_t c = *crc;
    for (size_t i = 0; i < len; ++i) c = c * 31u + data[i];
    *crc = c;
}
typedef struct bridge_pair { uint32_t lo; uint32_t hi; } bridge_pair;
int32_t bridge_split(uint64_t value, bridge_pair *out) {
    out->lo = (uint32_t)value;
    out->hi = (uint32_t)(value >> 32);
    return 1;
}
