/* C half of the wave-seven Bridge fixture: wire-struct slices, Option
 * slices and pointers, and functions that break single-element and
 * nocapture assumptions on purpose. */
#ifndef VSYS_REST_H
#define VSYS_REST_H
#include <stddef.h>
#include <stdint.h>

typedef struct rest_rec {
    uint32_t id;
    uint32_t flags;
    int64_t value;
} rest_rec_t;

struct rest_pair {
    uint16_t tag;
    uint16_t kind;
    rest_rec_t rec;
};

int64_t rest_sum(const rest_rec_t *recs, size_t n);
void rest_bump(rest_rec_t *recs, size_t n, uint32_t flag);
/* NULL with length 0 is accepted and returns 0xffffffff. */
uint32_t rest_checksum(const uint8_t *data, size_t len);
/* NULL returns -1; otherwise writes 7 and returns 0. */
int32_t rest_read(uint32_t *out);
void rest_fill_one(uint32_t *out);
/* Bug on purpose: writes out[0] and out[1]. */
void rest_fill_two(uint32_t *out);
/* Bug on purpose: writes recs[n], one past the declared length. */
void rest_overrun(rest_rec_t *recs, size_t n);
/* Returns a pointer into xs (derived from a nocapture borrow), or NULL. */
const uint32_t *rest_find(const uint32_t *xs, size_t n, uint32_t v);
const uint32_t *rest_table(void);
#endif
