/* legacy_codec.h: a small C batch-header codec, the C half of the
 * vesper migrate worked example (design section 29.3). */
#ifndef LEGACY_CODEC_H
#define LEGACY_CODEC_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef uint32_t codec_status_t;

enum codec_status {
    CODEC_OK = 0,
    CODEC_SHORT = 1,
    CODEC_BAD_MAGIC = 0x2,
    CODEC_LAST
};

/* Native in-memory form of a decoded batch header; no implicit padding. */
typedef struct batch_header {
    int64_t base_offset;
    int32_t batch_length;
    int32_t leader_epoch;
    uint8_t magic;
    uint8_t reserved[3];
    uint32_t crc;
} batch_header_t;

struct decoder_stats {
    uint32_t last_crc;
    int32_t record_count;
    batch_header_t last;
};

/* uint8_t followed by uint32_t: three bytes of implicit padding. */
struct padded_record {
    uint8_t tag;
    uint32_t value;
};

/* Pointers a header does not declare nonnull are lifted as Option. */
int32_t legacy_parse_batch_header(const uint8_t *raw_bytes, size_t raw_len,
                                  uint32_t *out_crc, int32_t *out_count)
    __attribute__((nonnull(1, 3, 4)));
/* Slice form of the same entry point: out[0] = crc, out[1] = count. */
int32_t legacy_parse_batch_header_into(const uint8_t *raw_bytes, size_t raw_len,
                                       uint32_t *out, size_t out_len)
    __attribute__((nonnull));
uint32_t legacy_crc32c(const uint8_t *data, size_t len, uint32_t seed);
size_t legacy_encode(const uint8_t src[], size_t src_len, uint8_t *dst, size_t dst_len);
codec_status_t legacy_status(void);
unsigned int legacy_version(void);
void legacy_reset(void);
const uint32_t *legacy_crc_table(void);
/* Single-element out-parameters: lifted to nocapture single pointers. */
int32_t legacy_read_header(const uint8_t *raw_bytes, size_t raw_len,
                           batch_header_t *out_hdr) __attribute__((nonnull(1, 3)));
int32_t legacy_fill(struct decoder_stats *stats);
/* Slices of wire-laid-out structs. */
int64_t legacy_sum_headers(const batch_header_t *headers, size_t count)
    __attribute__((nonnull));
void legacy_stamp_headers(batch_header_t *headers, size_t count, uint32_t crc);

/* Refused: each line exercises one typed refusal. */
void legacy_swap(uint32_t *a, uint32_t *b);
uint32_t legacy_sum4(const uint32_t values[4]);
void legacy_set_name(const uint8_t *name);
void legacy_attach(void *context);
int32_t legacy_pack(struct padded_record *record);
void legacy_pack_all(struct padded_record *records, size_t count);
long legacy_offset(void);
double legacy_ratio(uint32_t a);
int legacy_log(const char *fmt, ...);
void legacy_register(void (*callback)(int32_t));
enum codec_status legacy_last_status(void);
void legacy_flush();
static inline uint32_t legacy_inline_helper(uint32_t x) { return x + 1; }

union legacy_word {
    uint32_t word;
    uint8_t bytes[4];
};

extern uint32_t legacy_global_counter;

#ifdef __cplusplus
}
#endif

#endif /* LEGACY_CODEC_H */
