/* The clock a picolibc program reads: `gettimeofday`, which picolibc's
 * `time()` calls, and `clock_gettime`, which picolibc declares and nothing
 * defined, for every clock id its <time.h> names: CLOCK_REALTIME and
 * CLOCK_MONOTONIC, and under _GNU_SOURCE the coarse, raw and boot-time
 * variants. All of them are one source here. Step 5 of docs/roadmap/roadmap-c-hosting.md, since
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

/* picolibc's <time.h> names CLOCK_MONOTONIC only where _POSIX_MONOTONIC_CLOCK
 * is defined, which its features.h does for RTEMS alone; the Makefile's
 * PICO_INC defines it for every picolibc program here, since the clock
 * exists. Without it this file would not build, which is the point. */
#ifndef CLOCK_MONOTONIC
#error "CLOCK_MONOTONIC hidden: build with the Makefile's PICO_INC"
#endif

static unsigned long uptime_us(void) {
    return (unsigned long)__os_syscall1(SYS_MONOTONIC_US, 0);
}

int gettimeofday(struct timeval *restrict tv, void *restrict tz) {
    /* POSIX leaves a non-null tz unspecified, and BSD-style programs read it:
     * UTC with no daylight time, the only zone there is (the review of #224). */
    if (tz) {
        struct timezone *z = tz;
        z->tz_minuteswest = 0;
        z->tz_dsttime = 0;
    }
    if (tv) {
        unsigned long us = uptime_us();
        tv->tv_sec = (time_t)(us / 1000000UL);
        tv->tv_usec = (suseconds_t)(us % 1000000UL);
    }
    return 0;
}

int clock_gettime(clockid_t id, struct timespec *ts) {
    /* By number, as picolibc's <time.h> gives them: realtime-coarse 0,
     * realtime 1, monotonic 4, monotonic-raw 5, monotonic-coarse 6, boottime
     * 7. The ids from 2 and 3 (process and thread CPU time) and 8 (an alarm)
     * are not this clock, so they are refused. */
    if (id != 0 && id != CLOCK_REALTIME && (id < CLOCK_MONOTONIC || id > 7)) {
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
