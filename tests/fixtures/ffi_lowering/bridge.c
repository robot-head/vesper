/* A real C library the Vesper test program calls through Bridge (§13). */
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

/* Vesper code reports what it observed back into C. */
void bridge_report(uint64_t tag, uint64_t value) {
    printf("%llu=%llu\n", (unsigned long long)tag, (unsigned long long)value);
}
void bridge_report8(uint64_t tag, uint8_t value) {
    printf("%llu=%u\n", (unsigned long long)tag, (unsigned)value);
}
void bridge_report32(uint64_t tag, uint32_t value) {
    printf("%llu=%u\n", (unsigned long long)tag, (unsigned)value);
}
void bridge_report_signed(uint64_t tag, int64_t value) {
    printf("%llu=%lld\n", (unsigned long long)tag, (long long)value);
}

uint32_t bridge_sum(uint32_t a, uint32_t b) { return a + b; }
int32_t bridge_negative(void) { return -17; }
uint16_t bridge_wide(void) { return 65535; }
int8_t bridge_small(void) { return -5; }

/* Logical bool/enum refinements use total-bit carriers, not C _Bool/enum. */
uint8_t bridge_bool(uint8_t which) { return which ? 2 : 1; }
uint32_t bridge_state(uint32_t raw) { return raw; }
uint32_t bridge_next(uint32_t state) { return state == 1 ? 7 : 9; }

static uint64_t ticks;
uint64_t bridge_tick(void) { return ++ticks; }

size_t bridge_copy(const uint8_t *src, size_t src_len, uint8_t *dst, size_t dst_len) {
    size_t count = src_len < dst_len ? src_len : dst_len;
    for (size_t i = 0; i < count; ++i) dst[i] = src[i] ^ 0x5a;
    return count;
}
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

/* wire struct Hdr { magic: u32be, version: u8, tag: ByteArray[3], length: u64 } */
typedef struct bridge_hdr {
    uint8_t magic[4];
    uint8_t version;
    uint8_t tag[3];
    uint64_t length;
} bridge_hdr;
static uint32_t be32(const uint8_t *b) {
    return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3];
}
uint64_t bridge_hdr_sum(const bridge_hdr *h) {
    return (uint64_t)be32(h->magic) + h->version + h->tag[0] + h->tag[1] + h->tag[2] + h->length;
}
void bridge_hdr_bump(bridge_hdr *h) {
    uint32_t m = be32(h->magic) + 1;
    h->magic[0] = (uint8_t)(m >> 24);
    h->magic[1] = (uint8_t)(m >> 16);
    h->magic[2] = (uint8_t)(m >> 8);
    h->magic[3] = (uint8_t)m;
    h->version += 1;
    h->tag[0] ^= 0xff;
    h->length *= 2;
}

/* wire struct Rec { id: u64, value: i64 }: the same layout as the source struct. */
typedef struct bridge_rec { uint64_t id; int64_t value; } bridge_rec;
int64_t bridge_rec_total(const bridge_rec *r, size_t n) {
    int64_t total = 0;
    for (size_t i = 0; i < n; ++i) total += (int64_t)r[i].id * r[i].value;
    return total;
}
void bridge_rec_scale(bridge_rec *r, size_t n, uint64_t k) {
    for (size_t i = 0; i < n; ++i) r[i].value *= (int64_t)k;
}

/* Nullable pointers and slices. */
uint32_t bridge_opt_len(const uint8_t *data, size_t len) {
    if (!data) return len == 0 ? 999 : 998;
    return (uint32_t)len;
}
uint32_t bridge_opt_get(const uint32_t *p) { return p ? *p : 0xffffffffu; }
void bridge_opt_bump(uint32_t *p) { if (p) *p += 1; }
