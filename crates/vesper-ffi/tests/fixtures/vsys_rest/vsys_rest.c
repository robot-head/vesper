#include "vsys_rest.h"

int64_t rest_sum(const rest_rec_t *recs, size_t n) {
    int64_t total = 0;
    for (size_t i = 0; i < n; ++i) total += recs[i].value * (int64_t)recs[i].id;
    return total;
}
void rest_bump(rest_rec_t *recs, size_t n, uint32_t flag) {
    for (size_t i = 0; i < n; ++i) {
        recs[i].flags |= flag;
        recs[i].value += 1;
    }
}
uint32_t rest_checksum(const uint8_t *data, size_t len) {
    if (data == NULL) return 0xffffffffu;
    uint32_t c = 17;
    for (size_t i = 0; i < len; ++i) c = c * 31u + data[i];
    return c;
}
int32_t rest_read(uint32_t *out) {
    if (out == NULL) return -1;
    *out = 7;
    return 0;
}
void rest_fill_one(uint32_t *out) { out[0] = 11; }
void rest_fill_two(uint32_t *out) {
    out[0] = 11;
    out[1] = 12;
}
void rest_overrun(rest_rec_t *recs, size_t n) {
    for (size_t i = 0; i <= n; ++i) recs[i].flags = 0xdead;
}
const uint32_t *rest_find(const uint32_t *xs, size_t n, uint32_t v) {
    for (size_t i = 0; i < n; ++i)
        if (xs[i] == v) return &xs[i];
    return NULL;
}
const uint32_t *rest_table(void) {
    static const uint32_t table[4] = {1, 2, 3, 4};
    return table;
}
