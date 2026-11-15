/* C host for the isolated(mpk) test. Usage: mpk_host fault|ok|unset.
 * Tags one page with a fresh protection key, publishes the key's PKRU bits
 * through the Vesper object's exported mask symbol, and calls into Vesper. */
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef SEGV_PKUERR
#define SEGV_PKUERR 4
#endif

extern uint32_t vesper_bridge_pkru_revoke_Sandbox;
uint64_t sandbox_main(uint64_t scenario, uint64_t address);

static void on_segv(int sig, siginfo_t *info, void *context) {
    (void)sig;
    (void)context;
    if (info->si_code == SEGV_PKUERR) {
        static const char msg[] = "pkey fault\n";
        write(1, msg, sizeof msg - 1);
        _exit(42);
    }
    _exit(43);
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    int key = pkey_alloc(0, 0);
    if (key < 0) {
        printf("pku unavailable\n");
        return 77;
    }
    volatile uint64_t *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return 3;
    if (pkey_mprotect((void *)page, 4096, PROT_READ | PROT_WRITE, key) != 0) return 4;
    page[0] = 7;
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    if (strcmp(argv[1], "unset") != 0)
        vesper_bridge_pkru_revoke_Sandbox = 3u << (2 * key);
    uint64_t address = (uint64_t)(uintptr_t)page;
    if (strcmp(argv[1], "fault") == 0) {
        printf("before\n");
        fflush(stdout);
        sandbox_main(1, address);
        printf("no fault\n");
        return 0;
    }
    /* "ok" and "unset": isolated calls that never touch the page. */
    uint64_t r = sandbox_main(2, address);
    printf("result %llu\n", (unsigned long long)r);
    /* The trampoline restored PKRU: the host can still use its page. */
    page[0] += 1;
    printf("page %llu\n", (unsigned long long)page[0]);
    unsigned pkru = 0;
    __asm__ volatile("rdpkru" : "=a"(pkru) : "c"(0) : "rdx");
    printf("revoked after return %u\n", (pkru >> (2 * key)) & 3u);
    return 0;
}
