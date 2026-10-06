/* The clock in a C program: time, gettimeofday and clock_gettime, which
 * libc/pico/clock.c supplies from the kernel's MONOTONIC_US. Step 5 of
 * docs/roadmap/roadmap-c-hosting.md: until it, a program calling time() did
 * not link.
 *
 * Prints what it read, for scripts/test-cclock.py to compare with the host's
 * own clock and its own date formatting:
 *
 *     cclock: time=<time(NULL)>
 *     cclock: us=<gettimeofday, in microseconds>
 *     cclock: ctime=<ctime(&t), its newline dropped>
 *
 * then its own checks, one line each (`cclock: ok ...` or `cclock: FAIL
 * ...`), and `cclock: N checks, M failed`; exit 1 on any failure. Runs as
 * /bin/CCLOCK, through picolibc. */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>

/* picolibc's header hides it on this target (libc/pico/clock.c says why);
 * this is the number it would have. */
#ifndef CLOCK_MONOTONIC
#define CLOCK_MONOTONIC 4
#endif

static int g_checks;
static int g_failed;

static void check(const char *what, int ok) {
    g_checks++;
    if (!ok) {
        g_failed++;
    }
    printf("cclock: %s %s\r\n", ok ? "ok" : "FAIL", what);
}

int main(void) {
    struct timeval a, b;
    struct timespec rt, mono, mono2;
    time_t t = time(NULL);
    int got = gettimeofday(&a, NULL);
    int rr = clock_gettime(CLOCK_REALTIME, &rt);
    int mr = clock_gettime(CLOCK_MONOTONIC, &mono);
    int got2 = gettimeofday(&b, NULL);
    int mr2 = clock_gettime(CLOCK_MONOTONIC, &mono2);

    char when[32];
    strncpy(when, ctime(&t), sizeof when - 1);
    when[sizeof when - 1] = 0;
    when[strcspn(when, "\n")] = 0;
    printf("cclock: time=%ld\r\n", (long)t);
    printf("cclock: us=%ld\r\n", (long)a.tv_sec * 1000000L + (long)a.tv_usec);
    printf("cclock: ctime=%s\r\n", when);

    check("gettimeofday succeeds", got == 0 && got2 == 0);
    check("tv_usec is under a second", a.tv_usec >= 0 && a.tv_usec < 1000000 && b.tv_usec >= 0 &&
                                           b.tv_usec < 1000000);
    check("gettimeofday does not go back",
          b.tv_sec > a.tv_sec || (b.tv_sec == a.tv_sec && b.tv_usec >= a.tv_usec));
    check("time(NULL) is gettimeofday's second", a.tv_sec == t || a.tv_sec == t + 1);
    check("clock_gettime(CLOCK_REALTIME) succeeds, within a second of gettimeofday",
          rr == 0 && rt.tv_nsec >= 0 && rt.tv_nsec < 1000000000L &&
              (rt.tv_sec == a.tv_sec || rt.tv_sec == a.tv_sec + 1));
    check("clock_gettime(CLOCK_MONOTONIC) succeeds and does not go back",
          mr == 0 && mr2 == 0 &&
              (mono2.tv_sec > mono.tv_sec ||
               (mono2.tv_sec == mono.tv_sec && mono2.tv_nsec >= mono.tv_nsec)));
    errno = 0;
    check("clock_gettime of an unknown clock: EINVAL", clock_gettime(99, &rt) < 0 && errno == EINVAL);
    errno = 0;
    check("clock_gettime into a null timespec: EFAULT",
          clock_gettime(CLOCK_MONOTONIC, NULL) < 0 && errno == EFAULT);
    struct tm *g = gmtime(&t);
    check("gmtime says 1970, no wall clock yet", g && g->tm_year == 70);

    printf("cclock: %d checks, %d failed\r\n", g_checks, g_failed);
    return g_failed ? 1 : 0;
}
