/* poll for the keyboard in a C program (<poll.h>, libc/src/file.c), for
 * DevTools's note docs/handoffs/closed/2026-10-08-from-devtools-edit-poll.md. Runs as
 * /bin/CPOLL, through picolibc, driven by scripts/test-cpoll.py.
 *
 * `cpoll timing` prints one line per question, with nothing typed:
 *
 *     cpoll: zero 0                    (timeout 0: answers at once, none)
 *     cpoll: wait 0 ms=<n>             (timeout 200: none, after about 200 ms)
 *     cpoll: out 1 revents=4           (fd 1 is ready for POLLOUT)
 *     cpoll: closed 1 revents=32       (fd 9, not open, is POLLNVAL)
 *     cpoll: both 1 key=0 out=4 ms=<n> (fd 1 ready: fd 0 looked at, no wait)
 *     cpoll: sleep 0 ms=<n>            (no fds, timeout 100: a sleep)
 *     cpoll: null EFAULT
 *     cpoll: timing done
 *
 * `cpoll key` writes `cpoll: waiting` with write(), no newline (so only
 * poll's flush of the C library's buffer for fd 1 shows it), waits with
 * timeout -1, and prints `cpoll: key <n>` for the byte the read after it
 * returns; then waits again with timeout 3000 and
 * prints `cpoll: key <n> in time`, or `cpoll: none` when the time runs out;
 * then `cpoll: type now`, a two-second sleep for a key to be typed, and a poll
 * of fds 0 and 1 together, `cpoll: both-typed 2 key=1 out=4` when fd 0 is
 * answered rather than skipped beside the ready fd 1.
 *
 * `cpoll leave` polls fd 0 for up to 10 s, prints `cpoll: leave <n>` and exits
 * without reading: the key it saw must reach the shell. */
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include "probe_errno.h"

static long ms_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int timing(void) {
    struct pollfd p = {0, POLLIN, 0};
    printf("cpoll: zero %d\r\n", poll(&p, 1, 0));
    long t0 = ms_now();
    int n = poll(&p, 1, 200);
    printf("cpoll: wait %d ms=%ld\r\n", n, ms_now() - t0);
    struct pollfd q = {1, POLLOUT, 0};
    n = poll(&q, 1, 0);
    printf("cpoll: out %d revents=%d\r\n", n, q.revents);
    struct pollfd r = {9, POLLIN, 0};
    n = poll(&r, 1, 0);
    printf("cpoll: closed %d revents=%d\r\n", n, r.revents);
    struct pollfd two[2] = {{0, POLLIN, 0}, {1, POLLOUT, 0}};
    t0 = ms_now();
    n = poll(two, 2, 2000);
    printf("cpoll: both %d key=%d out=%d ms=%ld\r\n", n, two[0].revents, two[1].revents, ms_now() - t0);
    t0 = ms_now();
    n = poll(NULL, 0, 100);
    printf("cpoll: sleep %d ms=%ld\r\n", n, ms_now() - t0);
    errno = 0;
    n = poll(NULL, 1, 0);
    printf("cpoll: null %s\r\n", n == -1 ? err_name(errno) : "succeeded");
    printf("cpoll: timing done\r\n");
    return 0;
}

static int key(void) {
    struct pollfd p = {0, POLLIN, 0};
    unsigned char c;
    fflush(stdout);
    /* Through write(), as Edit draws its screen: the C library's own buffer
     * for fd 1, which poll must flush before it waits (stdio's buffer above
     * it is the program's to flush, as on Unix). */
    write(1, "cpoll: waiting", 14);
    if (poll(&p, 1, -1) == 1 && (p.revents & POLLIN) && read(0, &c, 1) == 1) {
        printf("\r\ncpoll: key %u\r\n", c);
    } else {
        printf("\r\ncpoll: FAIL first poll\r\n");
        return 1;
    }
    fflush(stdout);
    if (poll(&p, 1, 3000) == 1 && read(0, &c, 1) == 1) {
        printf("cpoll: key %u in time\r\n", c);
    } else {
        printf("cpoll: none\r\n");
    }
    /* A key typed during a sleep, then fd 0 and fd 1 together: fd 1 is ready
     * at once, and fd 0 must be answered POLLIN too, not skipped. */
    printf("cpoll: type now\r\n");
    fflush(stdout);
    poll(NULL, 0, 2000);
    struct pollfd two[2] = {{0, POLLIN, 0}, {1, POLLOUT, 0}};
    int n = poll(two, 2, 0);
    printf("cpoll: both-typed %d key=%d out=%d\r\n", n, two[0].revents, two[1].revents);
    if (two[0].revents & POLLIN) {
        read(0, &c, 1);
    }
    return 0;
}

/* `cpoll leave`: waits for a key with poll and exits WITHOUT reading it. The
 * key must stay in the kernel's queue for the shell (a key taken by poll and
 * held in the program died with it). */
static int leave(void) {
    struct pollfd p = {0, POLLIN, 0};
    printf("cpoll: leave waiting\r\n");
    fflush(stdout);
    int n = poll(&p, 1, 10000);
    printf("cpoll: leave %d\r\n", n);
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "timing") == 0) {
        return timing();
    }
    if (argc > 1 && strcmp(argv[1], "key") == 0) {
        return key();
    }
    if (argc > 1 && strcmp(argv[1], "leave") == 0) {
        return leave();
    }
    printf("usage: cpoll timing|key|leave\r\n");
    return 2;
}
