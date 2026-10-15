/* cmalloc: picolibc's malloc checked, then timed, at four numbers of slots.
 *
 * Each count runs OPS operations at random slots: a live block is freed or,
 * one time in four, reallocated to a new size; an empty slot is filled by
 * malloc, or one time in eight by calloc, or one time in eight by memalign
 * at 16, 32 or 64 bytes. About half the slots hold a block at any time.
 *
 * The check, untimed. Every block gets its slot's own byte written over its
 * whole length when it is handed out; a realloc'd block must keep that byte
 * over the part it kept, a calloc'd one must read 0 in every byte before it
 * is written, and a memalign'd one must be aligned. At the end every live
 * block must still hold its byte throughout, which two live blocks handed
 * out over the same memory could not. The calloc check is there because
 * newlib's calloc faulted while picolibc was built without
 * -fno-builtin-malloc, and memalign because nano-malloc's drops its size
 * store without the same flag (libc/picolibc-cross.txt). Without either flag
 * nano-malloc frees a chunk with no size and a later call loops for good:
 * then cmalloc never ends, which is how scripts/test-malloc.py sees it.
 *
 * The timing. One untimed pass first, so the first count carries no start-up
 * cost; then malloc, calloc, realloc and free alone are timed, no memalign
 * and no checks, the random choices drawn before the clock starts, the
 * fastest of REPEATS runs kept. nano-malloc walks one address-ordered free
 * list, so its cost grows with the count (DevTools's note,
 * docs/handoffs/2026-10-14-from-devtools-nano-malloc.md); the last line gives
 * the largest count's cost over the smallest's. It is a measurement, not a
 * check: it is what an allocator change is to be weighed by.
 *
 * Build: `make cmalloc-bin`; runs as /bin/CMALLOC. */
#include <malloc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define OPS 20000
#define REPEATS 3
#define MAX_SLOTS 4000
static const long SLOT_COUNTS[] = {500, 1000, 2000, MAX_SLOTS};
#define NCOUNTS ((int)(sizeof SLOT_COUNTS / sizeof SLOT_COUNTS[0]))

enum { OP_FREE, OP_REALLOC, OP_MALLOC, OP_CALLOC, OP_MEMALIGN };

/* One operation, drawn before it runs. Which half applies depends on the
 * slot at the time: `fill` to an empty slot, `empty` to a live one. */
struct op {
    unsigned short slot;
    unsigned char fill, empty;
    unsigned char size, align;
};

static unsigned char *blocks[MAX_SLOTS];
static unsigned char sizes[MAX_SLOTS];
static struct op ops[OPS];
static unsigned long seed = 12345;
static long failed, calloc_bad, misaligned, pattern_bad;

static unsigned long next(void) {
    seed = seed * 6364136223846793005UL + 1442695040888963407UL;
    return seed >> 33;
}

/* 16 to 128 bytes: the token and string sizes a preprocessor holds. */
static unsigned char size(void) {
    return (unsigned char)(16 + next() % 113);
}

static unsigned char mark(long slot) {
    return (unsigned char)(slot * 7 + 1);
}

static long now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (long)t.tv_sec * 1000000L + t.tv_nsec / 1000L;
}

static void draw(long slots, int with_memalign) {
    for (int k = 0; k < OPS; k++) {
        unsigned long f = next() % 8;
        ops[k].slot = (unsigned short)(next() % (unsigned long)slots);
        ops[k].fill = f == 0 ? OP_CALLOC : f == 1 && with_memalign ? OP_MEMALIGN : OP_MALLOC;
        ops[k].empty = next() % 4 == 0 ? OP_REALLOC : OP_FREE;
        ops[k].size = size();
        ops[k].align = (unsigned char)(16u << (next() % 3));
    }
}

/* A block just handed out for `slot`: checked as its kind requires, then its
 * slot's byte written over all of it. */
static void take(long slot, unsigned char *p, unsigned char n, int kind, unsigned char align, unsigned char kept) {
    blocks[slot] = p;
    sizes[slot] = p != NULL ? n : 0;
    if (p == NULL) {
        failed++;
        return;
    }
    for (long k = 0; kind == OP_CALLOC && k < n; k++) {
        calloc_bad += p[k] != 0;
    }
    for (long k = 0; kind == OP_REALLOC && k < kept; k++) {
        pattern_bad += p[k] != mark(slot);
    }
    misaligned += kind == OP_MEMALIGN && ((uintptr_t)p & (align - 1u)) != 0;
    memset(p, mark(slot), n);
}

static void check(long slots) {
    for (long i = 0; i < slots; i++) {
        take(i, malloc(16), 16, OP_MALLOC, 0, 0);
    }
    for (long i = 0; i < slots; i += 2) {
        free(blocks[i]);
        blocks[i] = NULL;
    }
    draw(slots, 1);
    for (int k = 0; k < OPS; k++) {
        const struct op *o = &ops[k];
        long i = o->slot;
        if (blocks[i] == NULL) {
            unsigned char *p = o->fill == OP_CALLOC ? calloc(1, o->size)
                             : o->fill == OP_MEMALIGN ? memalign(o->align, o->size)
                             : malloc(o->size);
            take(i, p, o->size, o->fill, o->align, 0);
        } else if (o->empty == OP_REALLOC) {
            unsigned char kept = sizes[i] < o->size ? sizes[i] : o->size;
            unsigned char *p = realloc(blocks[i], o->size);
            if (p == NULL) {
                failed++; /* the old block stays, still its slot's */
            } else {
                take(i, p, o->size, OP_REALLOC, 0, kept);
            }
        } else {
            free(blocks[i]);
            blocks[i] = NULL;
        }
    }
    for (long i = 0; i < slots; i++) {
        for (long k = 0; blocks[i] != NULL && k < sizes[i]; k++) {
            pattern_bad += blocks[i][k] != mark(i);
        }
        free(blocks[i]);
        blocks[i] = NULL;
    }
}

/* One timed run at `slots` slots: ns an operation; `*live` is the number of
 * slots holding a block when the timed loop ends. */
static long measure(long slots, long *live) {
    for (long i = 0; i < slots; i++) {
        blocks[i] = malloc(size());
        failed += blocks[i] == NULL;
    }
    for (long i = 0; i < slots; i += 2) {
        free(blocks[i]);
        blocks[i] = NULL;
    }
    draw(slots, 0);
    long start = now_us();
    for (int k = 0; k < OPS; k++) {
        const struct op *o = &ops[k];
        unsigned char **b = &blocks[o->slot];
        if (*b == NULL) {
            *b = o->fill == OP_CALLOC ? calloc(1, o->size) : malloc(o->size);
            failed += *b == NULL;
        } else if (o->empty == OP_REALLOC) {
            unsigned char *p = realloc(*b, o->size);
            if (p == NULL) {
                failed++;
            } else {
                *b = p;
            }
        } else {
            free(*b);
            *b = NULL;
        }
    }
    long us = now_us() - start;
    *live = 0;
    for (long i = 0; i < slots; i++) {
        *live += blocks[i] != NULL;
        free(blocks[i]);
        blocks[i] = NULL;
    }
    return us * 1000L / OPS;
}

int main(void) {
    for (int s = 0; s < NCOUNTS; s++) {
        if (SLOT_COUNTS[s] > MAX_SLOTS) {
            printf("cmalloc: %ld slots is more than MAX_SLOTS: FAIL\n", SLOT_COUNTS[s]);
            return 1;
        }
    }
    for (int s = 0; s < NCOUNTS; s++) {
        check(SLOT_COUNTS[s]);
    }
    printf("cmalloc: checked %d ops at each of %d counts: %ld failed, %ld calloc bytes not 0, "
           "%ld memalign misaligned, %ld bytes not their block's\n",
           OPS, NCOUNTS, failed, calloc_bad, misaligned, pattern_bad);
    int ok = failed == 0 && calloc_bad == 0 && misaligned == 0 && pattern_bad == 0;

    long ns[NCOUNTS], live = 0, skip;
    measure(SLOT_COUNTS[0], &skip); /* untimed: the start-up costs */
    for (int s = 0; s < NCOUNTS; s++) {
        ns[s] = -1;
        for (int r = 0; r < REPEATS; r++) {
            long l;
            long t = measure(SLOT_COUNTS[s], &l);
            if (ns[s] < 0 || t < ns[s]) {
                ns[s] = t;
                live = l;
            }
        }
        printf("cmalloc: %ld slots, %ld live at the end, %d ops, fastest of %d: %ld ns each\n",
               SLOT_COUNTS[s], live, OPS, REPEATS, ns[s]);
    }
    if (ns[0] > 0) {
        long ratio10 = ns[NCOUNTS - 1] * 10 / ns[0];
        printf("cmalloc: %ld slots cost %ld.%ld times %ld slots (measured, not graded): %s\n",
               SLOT_COUNTS[NCOUNTS - 1], ratio10 / 10, ratio10 % 10, SLOT_COUNTS[0], ok ? "ok" : "FAIL");
    } else {
        printf("cmalloc: %ld slots cost unmeasured (0 ns at %ld slots): %s\n",
               SLOT_COUNTS[NCOUNTS - 1], SLOT_COUNTS[0], ok ? "ok" : "FAIL");
    }
    return ok ? 0 : 1;
}
