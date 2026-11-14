/* C host: calls into the Vesper object, which calls the C libraries. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

uint64_t bridge_main(uint64_t scenario);

int main(void) {
    for (uint64_t s = 1; s <= 8; ++s) {
        uint64_t r = bridge_main(s);
        printf("scenario %llu -> %llu\n", (unsigned long long)s, (unsigned long long)r);
    }
    fflush(stdout);
    return 0;
}
