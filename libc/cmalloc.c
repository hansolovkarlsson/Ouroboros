/* cmalloc: what one malloc, free or realloc costs as the heap fills with
 * blocks, measured at four numbers of slots.
 *
 * For each of SLOT_COUNTS: allocate a block of a mixed small size into every
 * slot, writing every byte of it, free every other one (so the heap holds
 * about as many free blocks as live ones, scattered between them), then time
 * OPS operations at random slots: a live block is freed or, one time in four,
 * reallocated to a new size; an empty slot is allocated again, one time in
 * eight by calloc. About half the slots hold a block at any time; the line
 * for each count prints how many did at the end.
 *
 * Only the allocator is timed. The random choices are drawn before the clock
 * starts, and each calloc'd block still in its slot after the timed loop is
 * checked then, every byte of it 0. Its chunk was dirtied whole when first
 * handed out, so a calloc that clears too little leaves bytes set. The
 * calloc is there because newlib's calloc reads the size of the chunk malloc
 * just returned, and clang folded that read to a constant (memset of -8
 * bytes, the first calloc faulting) while picolibc was built without
 * -fno-builtin-malloc (libc/picolibc-cross.txt).
 *
 * One untimed pass runs first, so the first count measured carries no start-up
 * cost, and each count is measured REPEATS times and its fastest kept. An
 * allocator that walks its free blocks (picolibc's nano-malloc keeps one
 * address-ordered list, DevTools's note
 * docs/handoffs/closed/2026-10-14-from-devtools-nano-malloc.md) costs more per
 * operation as the slots grow; one that bins its free blocks by size costs
 * about the same at every count. The last line gives the ratio of the largest
 * count's cost to the smallest's, which is what scripts/test-malloc.py grades.
 * A cost that rounds to 0 ns cannot give a ratio: the line says so and the
 * program exits 1.
 *
 * Build: `make cmalloc-bin`; runs as /bin/CMALLOC. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define OPS 20000
#define REPEATS 3
#define MAX_SLOTS 4000
static const long SLOT_COUNTS[] = {500, 1000, 2000, 4000};
#define NCOUNTS ((int)(sizeof SLOT_COUNTS / sizeof SLOT_COUNTS[0]))

enum { OP_FREE, OP_REALLOC, OP_MALLOC, OP_CALLOC };

/* One operation, drawn before the clock starts. What it does depends on the
 * slot at the time: `fill` (malloc or calloc) applies to an empty slot,
 * `empty` (free or realloc) to a live one. */
struct op {
    unsigned short slot;
    unsigned char fill, empty;
    unsigned char size;
};

static void *blocks[MAX_SLOTS];
/* The size a slot's block was calloc'd at, or 0 when it was not. */
static unsigned char callocd[MAX_SLOTS];
static struct op ops[OPS];
static unsigned long seed = 12345;
static long failed, nonzero;

static unsigned long next(void) {
    seed = seed * 6364136223846793005UL + 1442695040888963407UL;
    return seed >> 33;
}

/* 16 to 128 bytes: the token and string sizes a preprocessor holds. */
static unsigned char size(void) {
    return (unsigned char)(16 + next() % 113);
}

static long now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (long)t.tv_sec * 1000000L + t.tv_nsec / 1000L;
}

/* One measurement at `slots` slots: ns an operation; `*live` is the number of
 * slots holding a block when the timed loop ends. */
static long measure(long slots, long *live) {
    for (long i = 0; i < slots; i++) {
        unsigned char n = size();
        blocks[i] = malloc(n);
        failed += blocks[i] == NULL;
        if (blocks[i] != NULL) {
            memset(blocks[i], 0xa5, n);
        }
        callocd[i] = 0;
    }
    for (long i = 0; i < slots; i += 2) {
        free(blocks[i]);
        blocks[i] = NULL;
    }
    for (int k = 0; k < OPS; k++) {
        ops[k].slot = (unsigned short)(next() % (unsigned long)slots);
        ops[k].fill = next() % 8 == 0 ? OP_CALLOC : OP_MALLOC;
        ops[k].empty = next() % 4 == 0 ? OP_REALLOC : OP_FREE;
        ops[k].size = size();
    }

    long start = now_us();
    for (int k = 0; k < OPS; k++) {
        const struct op *o = &ops[k];
        void **b = &blocks[o->slot];
        if (*b == NULL) {
            *b = o->fill == OP_CALLOC ? calloc(1, o->size) : malloc(o->size);
            failed += *b == NULL;
            callocd[o->slot] = o->fill == OP_CALLOC && *b != NULL ? o->size : 0;
        } else if (o->empty == OP_REALLOC) {
            void *p = realloc(*b, o->size);
            if (p == NULL) {
                failed++;
            } else {
                *b = p;
            }
            callocd[o->slot] = 0;
        } else {
            free(*b);
            *b = NULL;
            callocd[o->slot] = 0;
        }
    }
    long us = now_us() - start;

    *live = 0;
    for (long i = 0; i < slots; i++) {
        const unsigned char *p = blocks[i];
        *live += p != NULL;
        for (long k = 0; p != NULL && k < callocd[i]; k++) {
            nonzero += p[k] != 0;
        }
        free(blocks[i]);
        blocks[i] = NULL;
    }
    return us * 1000L / OPS;
}

int main(void) {
    long ns[NCOUNTS];
    long live;
    measure(SLOT_COUNTS[0], &live); /* untimed: the start-up costs */
    for (int s = 0; s < NCOUNTS; s++) {
        ns[s] = -1;
        for (int r = 0; r < REPEATS; r++) {
            long t = measure(SLOT_COUNTS[s], &live);
            if (ns[s] < 0 || t < ns[s]) {
                ns[s] = t;
            }
        }
        printf("cmalloc: %ld slots, %ld live at the end, %d ops, fastest of %d: %ld ns each\n",
               SLOT_COUNTS[s], live, OPS, REPEATS, ns[s]);
    }
    int measured = ns[0] > 0;
    if (measured) {
        long ratio10 = ns[NCOUNTS - 1] * 10 / ns[0];
        printf("cmalloc: %ld slots cost %ld.%ld times %ld slots, %ld failed, %ld calloc bytes not 0\n",
               SLOT_COUNTS[NCOUNTS - 1], ratio10 / 10, ratio10 % 10, SLOT_COUNTS[0], failed, nonzero);
    } else {
        printf("cmalloc: %ld slots cost unmeasured (0 ns at %ld slots), %ld failed, %ld calloc bytes not 0\n",
               SLOT_COUNTS[NCOUNTS - 1], SLOT_COUNTS[0], failed, nonzero);
    }
    return measured && failed == 0 && nonzero == 0 ? 0 : 1;
}
