/* Calls the fixture library with borrows placed inside larger arrays, so the
 * bytes just past each borrow are guard bytes this host owns, and prints the
 * raw arguments, returns and guard bytes for the Rust test to check. */
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include "vsys_rest.h"

static void hex(const void *p, size_t n) {
    const unsigned char *b = p;
    putchar(' ');
    for (size_t i = 0; i < n; ++i) printf("%02x", b[i]);
}
static unsigned long long addr(const void *p) { return (unsigned long long)(uintptr_t)p; }

int main(void) {
    rest_rec_t recs[3];
    memset(recs, 0, sizeof recs);
    recs[0] = (rest_rec_t){2, 0, 10};
    recs[1] = (rest_rec_t){3, 1, -4};
    memset(&recs[2], 0xa5, sizeof recs[2]);
    printf("sum %llu 2 %" PRId64 "\n", addr(recs), rest_sum(recs, 2));

    rest_rec_t before = recs[2];
    rest_bump(recs, 2, 0x10);
    printf("bump %llu 2", addr(recs));
    hex(&before, sizeof before);
    hex(&recs[2], sizeof recs[2]);
    printf(" %u %u %" PRId64 " %" PRId64 "\n", recs[0].flags, recs[1].flags, recs[0].value,
           recs[1].value);

    printf("checksum_null %u\n", rest_checksum(NULL, 0));
    uint8_t bytes[3] = {1, 2, 3};
    printf("checksum %llu 3 %u\n", addr(bytes), rest_checksum(bytes, 3));

    printf("read_null %d\n", rest_read(NULL));
    uint32_t cell = 0;
    int32_t r = rest_read(&cell);
    printf("read %llu %d %u\n", addr(&cell), r, cell);

    uint32_t one[2] = {0, 0xa5a5a5a5u};
    uint32_t guard_one = one[1];
    rest_fill_one(one);
    printf("fill_one %llu", addr(one));
    hex(&guard_one, 4);
    hex(&one[1], 4);
    printf(" %u\n", one[0]);

    uint32_t two[2] = {0, 0xa5a5a5a5u};
    uint32_t guard_two = two[1];
    rest_fill_two(two);
    printf("fill_two %llu", addr(two));
    hex(&guard_two, 4);
    hex(&two[1], 4);
    putchar('\n');

    rest_rec_t over[3];
    memset(over, 0, sizeof over);
    memset(&over[2], 0xa5, sizeof over[2]);
    rest_rec_t guard_over = over[2];
    rest_overrun(over, 2);
    printf("overrun %llu 2", addr(over));
    hex(&guard_over, sizeof guard_over);
    hex(&over[2], sizeof over[2]);
    putchar('\n');

    uint32_t xs[4] = {5, 6, 7, 8};
    printf("find %llu 4 %llu\n", addr(xs), addr(rest_find(xs, 4, 7)));
    printf("table %llu\n", addr(rest_table()));
    return 0;
}
