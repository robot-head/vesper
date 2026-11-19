/* Host program for the SH0 lane 4 pilot: calls the functions Vesper built. */
#include <stdint.h>
#include <stdio.h>

int64_t pilot_arith(int64_t a, int64_t b);
int64_t pilot_sum(int64_t n);
int64_t pilot_call(int64_t x);

int main(void) {
    printf("%lld %lld %lld %lld %lld %lld %lld\n",
           (long long)pilot_arith(7, 5),
           (long long)pilot_arith(-9, (int64_t)1 << 40),
           (long long)pilot_sum(0),
           (long long)pilot_sum(10),
           (long long)pilot_sum(1000),
           (long long)pilot_call(4),
           (long long)pilot_call(7));
    return 0;
}
