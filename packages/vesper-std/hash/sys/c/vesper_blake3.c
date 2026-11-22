/*
 * The `.vsys` library `Blake3` of @vesper-std/hash-sys over the official
 * BLAKE3 C implementation (Rust-exit SH6 lane 17, #1412). It replaces the
 * Rust shim over the `blake3` crate: the same symbols and the same results,
 * with no Rust between Vesper and C.
 *
 * A `Hasher` handle is a heap `blake3_hasher`: the constructors return a
 * `malloc`ed hasher (or NULL), `vesper_blake3_free` frees it once. The
 * slices are `(ptr, len)` pairs lent for the call.
 */
#include <stdint.h>
#include <stdlib.h>

#include "blake3.h"

static blake3_hasher *fresh(void) { return (blake3_hasher *)malloc(sizeof(blake3_hasher)); }

/* Whether `s[0..len)` is UTF-8 (the rules of Rust's `str::from_utf8`:
 * shortest forms only, no surrogates, nothing above U+10FFFF). */
static int is_utf8(const uint8_t *s, size_t len) {
  size_t i = 0;
  while (i < len) {
    uint8_t b = s[i];
    size_t n;
    uint32_t c;
    if (b < 0x80) {
      i += 1;
      continue;
    } else if (b >= 0xc2 && b <= 0xdf) {
      n = 1;
      c = b & 0x1f;
    } else if (b >= 0xe0 && b <= 0xef) {
      n = 2;
      c = b & 0x0f;
    } else if (b >= 0xf0 && b <= 0xf4) {
      n = 3;
      c = b & 0x07;
    } else {
      return 0;
    }
    if (len - i <= n) {
      return 0;
    }
    for (size_t k = 1; k <= n; k++) {
      uint8_t t = s[i + k];
      if ((t & 0xc0) != 0x80) {
        return 0;
      }
      c = (c << 6) | (t & 0x3f);
    }
    if ((n == 2 && c < 0x800) || (n == 3 && c < 0x10000) || c > 0x10ffff ||
        (c >= 0xd800 && c <= 0xdfff)) {
      return 0;
    }
    i += n + 1;
  }
  return 1;
}

blake3_hasher *vesper_blake3_new(void) {
  blake3_hasher *h = fresh();
  if (h != NULL) {
    blake3_hasher_init(h);
  }
  return h;
}

/* NULL unless `key` is exactly 32 bytes. */
blake3_hasher *vesper_blake3_new_keyed(const uint8_t *key, size_t len) {
  if (len != BLAKE3_KEY_LEN) {
    return NULL;
  }
  blake3_hasher *h = fresh();
  if (h != NULL) {
    blake3_hasher_init_keyed(h, key);
  }
  return h;
}

/* NULL unless `context` is UTF-8. */
blake3_hasher *vesper_blake3_new_derive_key(const uint8_t *context, size_t len) {
  if (!is_utf8(context, len)) {
    return NULL;
  }
  blake3_hasher *h = fresh();
  if (h != NULL) {
    blake3_hasher_init_derive_key_raw(h, context, len);
  }
  return h;
}

void vesper_blake3_update(blake3_hasher *h, const uint8_t *data, size_t len) {
  blake3_hasher_update(h, data, len);
}

/* `len` bytes of output from offset 0; the hasher keeps running. */
void vesper_blake3_finalize(const blake3_hasher *h, uint8_t *out, size_t len) {
  blake3_hasher_finalize(h, out, len);
}

void vesper_blake3_free(blake3_hasher *h) {
  free(h);
}

void vesper_blake3_hash(const uint8_t *data, size_t len, uint8_t *out, size_t out_len) {
  blake3_hasher h;
  blake3_hasher_init(&h);
  blake3_hasher_update(&h, data, len);
  blake3_hasher_finalize(&h, out, out_len);
}
