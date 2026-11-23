/* The NEON kernel (AArch64 only; blake3_impl.h turns NEON on for AArch64).
 * A `[package] objects` source of @vesper-std/hash-sys (manuscript §16.3).
 */
#include "vesper_b3_rename.h"
#include "blake3_neon.c"
