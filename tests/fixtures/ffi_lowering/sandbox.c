/* Foreign code called through an isolated(mpk) trampoline. bridge_poke
 * writes wherever it is told: a stray write into memory the host tagged
 * with the package's protection key must fault. */
#include <stdint.h>
void sandbox_poke(uint64_t address, uint64_t value) {
    *(volatile uint64_t *)(uintptr_t)address = value;
}
uint64_t sandbox_add(uint64_t a, uint64_t b) { return a + b; }
uint64_t sandbox_sum(const uint64_t *values, uint64_t len) {
    uint64_t total = 0;
    for (uint64_t i = 0; i < len; ++i) total += values[i];
    return total;
}
