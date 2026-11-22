// Calls one Vesper entry of xsk.ves (-DENTRY=name) and prints its value
// with the number of sockets and rings still live.
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

extern uint64_t xsk_live_count;
uint64_t ENTRY(void);

int main(void) {
    uint64_t value = ENTRY();
    printf("%" PRIu64 " %" PRIu64 "\n", value, xsk_live_count);
    return 0;
}
