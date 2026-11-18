// Calls one Vesper entry of aead.ves (-DENTRY=name) and prints its value
// with the library's creation and finalizer counts.
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

extern uint64_t aead_created;
extern uint64_t aead_freed;
uint64_t ENTRY(void);

int main(void) {
    uint64_t value = ENTRY();
    printf("%" PRIu64 " %" PRIu64 " %" PRIu64 "\n", value, aead_created, aead_freed);
    return 0;
}
