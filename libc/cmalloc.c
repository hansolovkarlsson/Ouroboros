/* cmalloc: what one malloc, free or realloc costs as the heap fills with
 * blocks, measured at four sizes of the live set.
 *
 * For each of LIVE_SIZES: allocate that many blocks of mixed small sizes,
 * free every other one (so the heap holds about as many free blocks as live
 * ones, scattered between them), then time OPS operations at random indices:
 * a live block is freed or, one time in four, reallocated to a new size; a
 * freed one is allocated again, one time in eight by calloc, whose bytes must
 * all read 0. Then everything is freed for the next size.
 *
 * The calloc is there because newlib's calloc reads the size of the chunk
 * malloc just returned, and clang folded that read to a constant (memset of
 * -8 bytes, the first calloc faulting) while picolibc was built without
 * -fno-builtin-malloc (libc/picolibc-cross.txt).
 *
 * An allocator that walks its free blocks (picolibc's nano-malloc keeps one
 * address-ordered list, DevTools's note
 * docs/handoffs/closed/2026-10-14-from-devtools-nano-malloc.md) costs more per
 * operation as the live set grows; one that bins its free blocks by size
 * costs about the same at every size. The last line gives the ratio of the
 * largest set's cost to the smallest's, which is what scripts/test-malloc.py
 * grades.
 *
 * Build: `make cmalloc-bin`; runs as /bin/CMALLOC. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define OPS 20000L
#define MAX_LIVE 4000
static const long LIVE_SIZES[] = {500, 1000, 2000, 4000};
#define NSIZES ((int)(sizeof LIVE_SIZES / sizeof LIVE_SIZES[0]))

static void *blocks[MAX_LIVE];
static unsigned long seed = 12345;

static unsigned long next(void) {
    seed = seed * 6364136223846793005UL + 1442695040888963407UL;
    return seed >> 33;
}

/* 16 to 128 bytes: the token and string sizes a preprocessor holds. */
static size_t size(void) {
    return 16 + next() % 113;
}

static long now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (long)t.tv_sec * 1000000L + t.tv_nsec / 1000L;
}

int main(void) {
    long ns[NSIZES];
    long failed = 0;
    long nonzero = 0;
    for (int s = 0; s < NSIZES; s++) {
        long live = LIVE_SIZES[s];
        for (long i = 0; i < live; i++) {
            blocks[i] = malloc(size());
            failed += blocks[i] == NULL;
            /* Dirty every block, so a calloc reusing one must clear it. */
            if (blocks[i] != NULL) {
                memset(blocks[i], 0xa5, 16);
            }
        }
        for (long i = 0; i < live; i += 2) {
            free(blocks[i]);
            blocks[i] = NULL;
        }
        long start = now_us();
        for (long op = 0; op < OPS; op++) {
            long i = (long)(next() % (unsigned long)live);
            if (blocks[i] == NULL && next() % 8 == 0) {
                size_t n = size();
                unsigned char *p = calloc(1, n);
                failed += p == NULL;
                for (size_t k = 0; p != NULL && k < n; k++) {
                    nonzero += p[k] != 0;
                }
                blocks[i] = p;
            } else if (blocks[i] == NULL) {
                blocks[i] = malloc(size());
                failed += blocks[i] == NULL;
            } else if (next() % 4 == 0) {
                void *p = realloc(blocks[i], size());
                if (p == NULL) {
                    failed++;
                } else {
                    blocks[i] = p;
                }
            } else {
                free(blocks[i]);
                blocks[i] = NULL;
            }
        }
        long us = now_us() - start;
        ns[s] = us * 1000L / OPS;
        printf("cmalloc: %ld live, %ld ops in %ld us, %ld ns each\n", live, OPS, us, ns[s]);
        for (long i = 0; i < live; i++) {
            free(blocks[i]);
            blocks[i] = NULL;
        }
    }
    long ratio10 = ns[0] > 0 ? ns[NSIZES - 1] * 10 / ns[0] : 0;
    printf("cmalloc: %ld live costs %ld.%ld times %ld live, %ld failed, %ld calloc bytes not 0\n",
           LIVE_SIZES[NSIZES - 1], ratio10 / 10, ratio10 % 10, LIVE_SIZES[0], failed, nonzero);
    return failed == 0 && nonzero == 0 ? 0 : 1;
}
