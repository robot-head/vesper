// The C side of tests/ffi_handles.rs's object test: the same toy stateful
// AEAD as the Rust shim in that test (a 256-byte key schedule, a keystream
// XOR and a 16-byte tag), with creation and finalizer counters.
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define TAG 16

typedef struct {
    uint8_t s[256];
    uint64_t key;
} sealer;

uint64_t aead_created;
uint64_t aead_freed;
static const uint64_t probe = 0x5eed;

static void expand(sealer *out, uint64_t key) {
    uint8_t k[8];
    for (int i = 0; i < 8; i++) {
        k[i] = (uint8_t)(key >> (8 * i));
    }
    for (int i = 0; i < 256; i++) {
        out->s[i] = (uint8_t)i;
    }
    uint8_t j = 0;
    for (int pass = 0; pass < 4; pass++) {
        for (int i = 0; i < 256; i++) {
            j = (uint8_t)(j + out->s[i] + k[i % 8]);
            uint8_t t = out->s[i];
            out->s[i] = out->s[j];
            out->s[j] = t;
        }
    }
    out->key = key;
}

static void keystream_xor(const sealer *sch, uint8_t *payload, size_t n) {
    uint8_t s[256];
    memcpy(s, sch->s, sizeof s);
    uint8_t i = 0, j = 0;
    for (size_t at = 0; at < n; at++) {
        i = (uint8_t)(i + 1);
        j = (uint8_t)(j + s[i]);
        uint8_t t = s[i];
        s[i] = s[j];
        s[j] = t;
        payload[at] ^= s[(uint8_t)(s[i] + s[j])];
    }
}

static void tag(const sealer *sch, const uint8_t *c, size_t n, uint8_t out[TAG]) {
    uint64_t a = 0xcbf29ce484222325ull ^ sch->key;
    uint64_t b = 0x9e3779b97f4a7c15ull;
    for (size_t i = 0; i < n; i++) {
        a = (a ^ c[i]) * 0x100000001b3ull;
        b = ((b << 5) | (b >> 59)) ^ a;
    }
    for (int i = 0; i < 8; i++) {
        out[i] = (uint8_t)(a >> (8 * i));
        out[8 + i] = (uint8_t)(b >> (8 * i));
    }
}

static uint32_t seal(const sealer *sch, uint8_t *p, size_t n) {
    if (n < TAG) {
        return 2;
    }
    keystream_xor(sch, p, n - TAG);
    tag(sch, p, n - TAG, p + n - TAG);
    return 0;
}

sealer *aead_sealer_new(uint64_t key) {
    if (key == 0) {
        return NULL;
    }
    sealer *s = malloc(sizeof *s);
    if (s == NULL) {
        abort();
    }
    expand(s, key);
    aead_created++;
    return s;
}

sealer *aead_sealer_must(uint64_t key) { return aead_sealer_new(key); }

uint32_t aead_seal(sealer *s, uint8_t *p, size_t n) { return seal(s, p, n); }

uint32_t aead_open(const sealer *s, uint8_t *p, size_t n) {
    if (n < TAG) {
        return 2;
    }
    uint8_t t[TAG];
    tag(s, p, n - TAG, t);
    if (memcmp(t, p + n - TAG, TAG) != 0) {
        return 1;
    }
    keystream_xor(s, p, n - TAG);
    return 0;
}

void aead_sealer_free(sealer *s) {
    aead_freed++;
    free(s);
}

uint32_t aead_seal_once(uint64_t key, uint8_t *p, size_t n) {
    if (p == NULL) {
        return 77;
    }
    sealer s;
    expand(&s, key);
    return seal(&s, p, n);
}

const uint64_t *aead_probe(void) { return &probe; }

uint64_t aead_probe_live(const uint64_t *p) {
    if (*p != probe) {
        abort();
    }
    return aead_created - aead_freed;
}
