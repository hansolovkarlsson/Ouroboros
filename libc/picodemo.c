/* picolibc demo: unmodified standard-C stdlib code running on Ouroboros through
 * the picolibc port (its libc.a + our syscall stubs in libc/src). This is code
 * that our hand-rolled libc could not run - %f/%e/%g float formatting (picolibc's
 * ryu), snprintf, qsort, strtol - the point of the port. Build: `make cpico-bin`;
 * runs as /bin/CPICO. See docs/processes.md's "Writing a program in C". */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int cmp_int(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

int main(void) {
    printf("=== picolibc on Ouroboros ===\n");
    printf("integers: %d %u 0x%x 0%o\n", -42, 42u, 255, 64);
    printf("floats:   pi=%.5f  e=%.3e  g=%g\n", 3.14159265, 2.718281828, 1234567.0);
    printf("strings:  [%10s] [%-10s] [%.3s]\n", "right", "left", "truncated");

    char buf[64];
    snprintf(buf, sizeof buf, "%d items at $%.2f each", 3, 9.99);
    printf("snprintf: %s\n", buf);

    int v[6] = {5, 2, 8, 1, 9, 3};
    qsort(v, 6, sizeof v[0], cmp_int);
    printf("qsort:   ");
    for (int i = 0; i < 6; i++) {
        printf(" %d", v[i]);
    }
    printf("\n");

    int *heap = malloc(4 * sizeof(int));
    for (int i = 0; i < 4; i++) {
        heap[i] = (i + 1) * (i + 1);
    }
    printf("malloc:   %d %d %d %d\n", heap[0], heap[1], heap[2], heap[3]);
    free(heap);

    printf("strtol:   %ld\n", strtol("  -1234rest", NULL, 10));
    printf("done.\n");
    return 0;
}
