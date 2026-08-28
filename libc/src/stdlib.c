#include <stdlib.h>
#include <unistd.h>

void exit(int code) {
    _exit(code);
}

/* Minimal bump allocator over sbrk: fast, no per-object metadata, and free() is
 * a no-op (documented). Fine for short-lived programs; a real free-list is a
 * later refinement. 16-byte alignment (the AArch64 max_align). */
void *malloc(size_t n) {
    n = (n + 15u) & ~(size_t)15u;
    void *p = sbrk((long)n);
    if (p == (void *)-1) {
        return (void *)0;
    }
    return p;
}

void free(void *p) {
    (void)p; /* no-op: bump allocator */
}

int atoi(const char *s) {
    int v = 0;
    int sign = 1;
    while (*s == ' ') {
        s++;
    }
    if (*s == '-') {
        sign = -1;
        s++;
    } else if (*s == '+') {
        s++;
    }
    while (*s >= '0' && *s <= '9') {
        v = v * 10 + (*s - '0');
        s++;
    }
    return v * sign;
}
