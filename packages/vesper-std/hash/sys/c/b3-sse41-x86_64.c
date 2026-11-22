/* The SSE4.1 kernel (x86-64 only).
 * A `[package] objects` source of @vesper-std/hash-sys (manuscript §16.3).
 */
#include "vesper_b3_rename.h"
/* SSE4.1: the kernel is compiled for its own instructions only, and
 * blake3_dispatch.c calls it when the CPU has them. GCC and Clang take the
 * instruction set from the pragma, so no per-file flag is needed; MSVC
 * compiles the intrinsics without one. */
#if defined(__clang__)
#pragma clang attribute push(__attribute__((target("sse4.1"))), apply_to = function)
#elif defined(__GNUC__)
#pragma GCC target("sse4.1")
#endif
#include "blake3_sse41.c"
#if defined(__clang__)
#pragma clang attribute pop
#endif
