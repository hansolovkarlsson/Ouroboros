/* The clock a picolibc program reads: `gettimeofday`, which picolibc's
 * `time()` calls, and `clock_gettime` for CLOCK_REALTIME and CLOCK_MONOTONIC
 * (whose name picolibc hides here, see below), which picolibc declares and
 * nothing defined. Step 5 of docs/roadmap/roadmap-c-hosting.md, since
 * 2026-10-06; until then a program calling `time()` did not link.
 *
 * The kernel has no wall clock, only MONOTONIC_US, microseconds since boot. So
 * both clocks are that, and the realtime one reads as 1970 plus uptime:
 * `ctime(time(NULL))` says "Thu Jan  1 00:01:23 1970" a minute and a bit after
 * boot. Honest, and enough for the preprocessor, whose `__DATE__` is then
 * wrong but well-formed and which honours SOURCE_DATE_EPOCH (step 3) where a
 * real date matters. A wall clock from the platform's RTC (PL031 on QEMU) is
 * its own item on the roadmap.
 *
 * picolibc-only, in the port beside builtins.c: the hand-rolled libc has no
 * <time.h> or <sys/time.h>, so no program built on it can call these. Not
 * here yet: `times` (behind `clock()`), `clock_getres` and `nanosleep`. */
#include "sys.h"
#include <errno.h>
#include <sys/time.h>
#include <time.h>

/* picolibc's <time.h> names CLOCK_MONOTONIC only where its features.h defines
 * _POSIX_MONOTONIC_CLOCK, which is on RTEMS alone, so on this target a
 * program cannot spell it from the header. It is still answered, by the
 * number picolibc gives it, for a ported program that defines it itself. */
#ifndef CLOCK_MONOTONIC
#define CLOCK_MONOTONIC 4
#endif

static unsigned long uptime_us(void) {
    return (unsigned long)__os_syscall1(SYS_MONOTONIC_US, 0);
}

int gettimeofday(struct timeval *restrict tv, void *restrict tz) {
    (void)tz; /* POSIX leaves a non-null tz unspecified; there is no zone here */
    if (tv) {
        unsigned long us = uptime_us();
        tv->tv_sec = (time_t)(us / 1000000UL);
        tv->tv_usec = (suseconds_t)(us % 1000000UL);
    }
    return 0;
}

int clock_gettime(clockid_t id, struct timespec *ts) {
    if (id != CLOCK_REALTIME && id != CLOCK_MONOTONIC) {
        errno = EINVAL;
        return -1;
    }
    if (!ts) {
        errno = EFAULT;
        return -1;
    }
    unsigned long us = uptime_us();
    ts->tv_sec = (time_t)(us / 1000000UL);
    ts->tv_nsec = (long)(us % 1000000UL) * 1000L;
    return 0;
}
