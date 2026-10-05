/* cmem: the heap a C program gets, held through picolibc's malloc and checked.
 *
 * Mallocs 16 KiB blocks until malloc refuses, writes a pattern through every
 * byte of every block, then reads every byte back, and prints the heap's size
 * (HEAP_INFO) beside what malloc held. All the blocks are live at once, so
 * the figure is what a program like Proem can hold at its peak, less one
 * block's rounding. Build: `make cmem-bin`; runs as /bin/CMEM. Exits 0 when
 * every byte read back, 1 otherwise. */
#include <stdio.h>
#include <stdlib.h>
#include "include/sys.h"

#define BLOCK (16 * 1024)
#define MAX_BLOCKS 1024 /* 16 MiB of blocks, past any heap a slot can hold */

static unsigned char *blocks[MAX_BLOCKS];

static unsigned char pattern(long block, long i) {
    return (unsigned char)(block * 31 + i * 7 + 1);
}

int main(void) {
    long heap = __os_syscall1(SYS_HEAP_INFO, HEAP_INFO_SIZE);
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
    printf("cmem: heap %ld bytes, malloc held %ld bytes live in %ld blocks of %d, %ld bad\n",
           heap, n * (long)BLOCK, n, BLOCK, bad);
    for (long b = 0; b < n; b++) {
        free(blocks[b]);
    }
    return bad == 0 ? 0 : 1;
}
