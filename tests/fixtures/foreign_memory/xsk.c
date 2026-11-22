// The C side of xsk.vsys: an AF_XDP-shaped socket and kernel stand-in. The
// socket keeps the UMEM address it is given and writes received frames
// into it during later calls; the receive ring is an anonymous mmap that
// the socket owns and xsk_ring_unmap releases with munmap.
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

#define FRAME 2048u
#define FRAMES 16u
#define ENTRIES 8u
#define RING_BYTES (64u + 16u * ENTRIES)

struct socket {
    uint8_t *umem;
    size_t umem_len;
    uint32_t frame_size;
    uint8_t *ring;
    uint32_t received;
};

uint64_t xsk_live_count = 0;

struct socket *xsk_socket_new(uint8_t *umem, size_t umem_len, uint32_t frame_size) {
    if (umem == NULL || umem_len < (size_t)FRAME * FRAMES) {
        return NULL;
    }
    struct socket *s = calloc(1, sizeof *s);
    if (s == NULL) {
        return NULL;
    }
    s->umem = umem;
    s->umem_len = umem_len;
    s->frame_size = frame_size;
    xsk_live_count++;
    return s;
}

void *xsk_ring_map(struct socket *s, uint64_t bytes) {
    if (bytes != RING_BYTES) {
        return MAP_FAILED;
    }
    uint8_t *ring = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (ring == MAP_FAILED) {
        return MAP_FAILED;
    }
    for (size_t i = 8; i < RING_BYTES; i++) {
        ring[i] = (uint8_t)(i * 7 % 251);
    }
    s->ring = ring;
    xsk_live_count++;
    return ring;
}

static uint32_t read32(const uint8_t *p) {
    uint32_t v;
    memcpy(&v, p, 4);
    return v;
}

uint32_t xsk_kernel_rx(struct socket *s, uint32_t packets) {
    uint32_t got = 0;
    for (uint32_t n = 0; n < packets; n++) {
        uint32_t producer = read32(s->ring);
        uint32_t consumer = read32(s->ring + 4);
        if (producer - consumer == ENTRIES) {
            break;
        }
        uint32_t k = s->received;
        uint64_t addr = (uint64_t)(k % FRAMES) * s->frame_size;
        uint32_t len = 64u + (k % 7u) * 100u;
        for (uint32_t i = 0; i < len; i++) {
            s->umem[addr + i] = (uint8_t)(k * 31u + i);
        }
        uint8_t *desc = s->ring + 64u + 16u * (producer % ENTRIES);
        memcpy(desc, &addr, 8);
        memcpy(desc + 8, &len, 4);
        producer++;
        memcpy(s->ring, &producer, 4);
        s->received++;
        got++;
    }
    return got;
}

int32_t xsk_ring_unmap(void *ring, size_t len) {
    xsk_live_count--;
    return munmap(ring, len);
}

void xsk_socket_free(struct socket *s) {
    xsk_live_count--;
    free(s);
}

uint64_t xsk_live(void) {
    return xsk_live_count;
}
