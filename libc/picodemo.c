/* picolibc demo: unmodified standard-C stdlib code running on Ouroboros through
 * the picolibc port (its libc.a + our syscall stubs in libc/src). This is code
 * that our hand-rolled libc could not run - %f/%e/%g float formatting (picolibc's
 * ryu), snprintf, qsort, strtol - the point of the port. Build: `make cpico-bin`;
 * runs as /bin/CPICO. See docs/processes.md's "Writing a program in C".
 *
 * `cpico fenv` instead checks the floating-point exception flags picolibc's
 * math leaves, for scripts/test-cfenv.py: one line each (`cpico: ok ...` or
 * `cpico: FAIL ...`), then `cpico: N checks, M failed`, exit 1 on any failure.
 * Until 2026-10-16 picolibc was built without -frounding-math, and clang
 * computed both arms of a choice and selected one, so round(1e308) and
 * fdim(-1e308, 1e308) overflowed inside the function and returned the right
 * value with OVERFLOW and INEXACT set. The first three checks are the
 * observer's own: flags that clear, and two operations that must raise them.
 * The mode lives here, not in a program of its own, because /bin's listing is
 * nearly at its 512-byte reply (ROADMAP.md, the paged NP_READDIR item). */
#include <fenv.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int cmp_int(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

static int g_checks;
static int g_failed;

/* The flags as names, "none" for none: what the check line prints. */
static const char *flag_names(int f, char *buf, size_t len) {
    buf[0] = 0;
    if (f & FE_INVALID) strncat(buf, " INVALID", len - strlen(buf) - 1);
    if (f & FE_DIVBYZERO) strncat(buf, " DIVBYZERO", len - strlen(buf) - 1);
    if (f & FE_OVERFLOW) strncat(buf, " OVERFLOW", len - strlen(buf) - 1);
    if (f & FE_UNDERFLOW) strncat(buf, " UNDERFLOW", len - strlen(buf) - 1);
    if (f & FE_INEXACT) strncat(buf, " INEXACT", len - strlen(buf) - 1);
    return buf[0] ? buf + 1 : "none";
}

/* One check: the value is `want` (both infinite of one sign counts) and
 * exactly the flags `want_flags` were raised getting it. */
static void check(const char *what, double got, double want, int flags, int want_flags) {
    char have[64], need[64];
    int ok = (got == want) && flags == want_flags;
    g_checks++;
    if (!ok) {
        g_failed++;
    }
    printf("cpico: %s %s = %g, flags %s (want %g, flags %s)\n", ok ? "ok" : "FAIL", what, got,
           flag_names(flags, have, sizeof have), want, flag_names(want_flags, need, sizeof need));
}

static int fenv_checks(void) {
    /* volatile, so the program's own compiler cannot fold an operation away
     * or compute it before the flags are cleared. */
    volatile double big = 1e308, ten = 10.0, one = 1.0, three = 3.0, r;
    int f;

    feraiseexcept(FE_OVERFLOW | FE_INEXACT);
    feclearexcept(FE_ALL_EXCEPT);
    f = fetestexcept(FE_ALL_EXCEPT);
    check("feclearexcept clears", 0.0, 0.0, f, 0);

    feclearexcept(FE_ALL_EXCEPT);
    r = big * ten;
    f = fetestexcept(FE_ALL_EXCEPT);
    check("1e308 * 10", r, HUGE_VAL, f, FE_OVERFLOW | FE_INEXACT);

    feclearexcept(FE_ALL_EXCEPT);
    r = one / three;
    f = fetestexcept(FE_ALL_EXCEPT);
    check("1 / 3", r, 1.0 / 3.0, f, FE_INEXACT);

    feclearexcept(FE_ALL_EXCEPT);
    r = round(big);
    f = fetestexcept(FE_ALL_EXCEPT);
    check("round(1e308)", r, 1e308, f, 0);

    feclearexcept(FE_ALL_EXCEPT);
    r = round(-big);
    f = fetestexcept(FE_ALL_EXCEPT);
    check("round(-1e308)", r, -1e308, f, 0);

    feclearexcept(FE_ALL_EXCEPT);
    r = fdim(-big, big);
    f = fetestexcept(FE_ALL_EXCEPT);
    check("fdim(-1e308, 1e308)", r, 0.0, f, 0);

    /* A real overflow in fdim is still reported. */
    feclearexcept(FE_ALL_EXCEPT);
    r = fdim(big, -big);
    f = fetestexcept(FE_ALL_EXCEPT);
    check("fdim(1e308, -1e308)", r, HUGE_VAL, f, FE_OVERFLOW | FE_INEXACT);

    printf("cpico: %d checks, %d failed\n", g_checks, g_failed);
    return g_failed ? 1 : 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "fenv") == 0) {
        return fenv_checks();
    }
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
