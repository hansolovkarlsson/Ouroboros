/* poll for the keyboard in a C program (<poll.h>, libc/src/file.c), for
 * DevTools's note docs/handoffs/2026-10-08-from-devtools-edit-poll.md. Runs as
 * /bin/CPOLL, through picolibc, driven by scripts/test-cpoll.py.
 *
 * `cpoll timing` prints one line per question, with nothing typed:
 *
 *     cpoll: zero 0                    (timeout 0: answers at once, none)
 *     cpoll: wait 0 ms=<n>             (timeout 200: none, after about 200 ms)
 *     cpoll: other 1 revents=32        (fd 1 is POLLNVAL at once)
 *     cpoll: sleep 0 ms=<n>            (no fds, timeout 100: a sleep)
 *     cpoll: null EFAULT
 *     cpoll: timing done
 *
 * `cpoll key` writes `cpoll: waiting` with write(), no newline (so only
 * poll's flush of the C library's buffer for fd 1 shows it), waits with
 * timeout -1, and prints `cpoll: key <n>` for the byte the read after it
 * returns; then waits again with timeout 3000 and
 * prints `cpoll: key <n> in time`, or `cpoll: none` when the time runs out. */
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
    struct pollfd q = {1, POLLIN, 0};
    n = poll(&q, 1, 0);
    printf("cpoll: other %d revents=%d\r\n", n, q.revents);
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
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "timing") == 0) {
        return timing();
    }
    if (argc > 1 && strcmp(argv[1], "key") == 0) {
        return key();
    }
    printf("usage: cpoll timing|key\r\n");
    return 2;
}
