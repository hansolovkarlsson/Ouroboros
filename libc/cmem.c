/* cmem: the heap a C program gets, checked three ways.
 *
 * 1. Clean: before the first malloc, every byte of the heap (HEAP_INFO) reads
 *    0. The loader zeroes a region before loading into it; a runtime region is
 *    the slot the last task to exit gave back, so without that a second run of
 *    cmem would find the first run's pattern here.
 * 2. Big enough: the heap is at least CMEM_MIN_HEAP, the 1 MiB Proem asked for
 *    (docs/handoffs/, from Proem, heap-growth).
 * 3. Usable: picolibc's malloc holds 16 KiB blocks until it refuses, all live at
 *    once, within CMEM_SLACK of the heap; a pattern written through every byte
 *    of every block reads back.
 *
 * Prints one line with the figures and `ok` or `FAIL`, and exits 0 or 1.
 * Build: `make cmem-bin`; runs as /bin/CMEM; `make test-heap` runs it twice
 * in one boot. */
#include <stdio.h>
#include <stdlib.h>
#include "include/sys.h"

#define BLOCK (16 * 1024)
#define MAX_BLOCKS 1024 /* 16 MiB of blocks, past any heap a slot can hold */
#define CMEM_MIN_HEAP (1024L * 1024L)
/* What malloc may fall short of the heap by: its headers, its own rounding,
 * and the last block that did not fit. Measured at 16 KiB on a 1 MiB heap. */
#define CMEM_SLACK (64L * 1024L)

static unsigned char *blocks[MAX_BLOCKS];

static unsigned char pattern(long block, long i) {
    return (unsigned char)(block * 31 + i * 7 + 1);
}

int main(void) {
    const unsigned char *base = (const unsigned char *)__os_syscall1(SYS_HEAP_INFO, HEAP_INFO_BASE);
    long heap = __os_syscall1(SYS_HEAP_INFO, HEAP_INFO_SIZE);
    long dirty = 0;
    for (long i = 0; base != NULL && i < heap; i++) {
        if (base[i] != 0) {
            dirty++;
        }
    }

    long n = 0;
    while (n < MAX_BLOCKS) {
        unsigned char *p = malloc(BLOCK);
        if (p == NULL) {
            break;
        }
        blocks[n++] = p;
    }
    for (long b = 0; b < n; b++) {
        for (long i = 0; i < BLOCK; i++) {
            blocks[b][i] = pattern(b, i);
        }
    }
    long bad = 0;
    for (long b = 0; b < n; b++) {
        for (long i = 0; i < BLOCK; i++) {
            if (blocks[b][i] != pattern(b, i)) {
                bad++;
            }
        }
    }
    long held = n * (long)BLOCK;
    int ok = dirty == 0 && heap >= CMEM_MIN_HEAP && held + CMEM_SLACK >= heap && bad == 0;
    printf("cmem: heap %ld bytes, %ld nonzero at start, malloc held %ld bytes live in %ld blocks of %d, %ld bad: %s\n",
           heap, dirty, held, n, BLOCK, bad, ok ? "ok" : "FAIL");
    for (long b = 0; b < n; b++) {
        free(blocks[b]);
    }
    return ok ? 0 : 1;
}
