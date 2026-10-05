/* Hold a file open across `unmount` and `mount -a`: the check that a fid on an
 * unmounted filesystem stays allocated, dead, until its owner closes it
 * (`Fid::dead` in programs/servers/fsd/src/main.rs). A program because the
 * condition needs a file held open while the shell runs other commands: run it
 * with `exec`, then `unmount`, then `mount -a`. Built by `make cfidhold-bin`,
 * staged as /bin/CFIDHOLD; `scripts/test-unmount.py` drives it.
 *
 * It opens /etc/passwd, waits until a read on it fails (the unmount), waits
 * until /etc/passwd opens again (the remount), and then requires: the second
 * open got a different descriptor (a freed slot would have been handed back,
 * the old descriptor then reaching the new file), the old one still fails,
 * the new one reads, and closing the old one succeeds. Prints `cfidhold: ok`
 * or `cfidhold: FAIL (...)`. */
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
#include "sys.h"

#define POLL_TICKS 25   /* half a second at the 20 ms tick */
#define POLLS 120       /* a minute in all, per wait */

/* A read from the start: lseek then read (this libc has no pread). */
static long read_start(int fd, char *buf, long n) {
    if (lseek(fd, 0, 0) < 0) {
        return -1;
    }
    return read(fd, buf, n);
}

static void pause_ticks(long n) {
    long until = __os_syscall1(SYS_GET_TICKS, 0) + n;
    while (__os_syscall1(SYS_GET_TICKS, 0) < until) {
        __os_syscall1(SYS_YIELD, 0);
    }
}

int main(void) {
    char buf[16];
    int fd1 = open("/etc/passwd", O_RDONLY);
    if (fd1 < 0) {
        printf("cfidhold: FAIL (first open)\r\n");
        return 1;
    }
    printf("cfidhold: opened fd %d, waiting for unmount\r\n", fd1);
    int polls = 0;
    while (read_start(fd1, buf, sizeof buf) > 0) {
        if (++polls > POLLS) {
            printf("cfidhold: FAIL (no unmount seen)\r\n");
            return 1;
        }
        pause_ticks(POLL_TICKS);
    }
    printf("cfidhold: fd %d fails after unmount, waiting for mount\r\n", fd1);
    int fd2 = -1;
    for (polls = 0; fd2 < 0; polls++) {
        if (polls > POLLS) {
            printf("cfidhold: FAIL (no remount seen)\r\n");
            return 1;
        }
        pause_ticks(POLL_TICKS);
        fd2 = open("/etc/passwd", O_RDONLY);
    }
    long old_read = read_start(fd1, buf, sizeof buf);
    long new_read = read_start(fd2, buf, sizeof buf);
    int closed_old = close(fd1);
    /* %d with int casts: this libc's printf has no %ld. */
    printf("cfidhold: after mount fd %d, old fd %d reads %d, new reads %d, old closes %d\r\n",
           fd2, fd1, (int)old_read, (int)new_read, closed_old);
    if (fd2 == fd1 || old_read >= 0 || new_read <= 0 || closed_old != 0) {
        printf("cfidhold: FAIL\r\n");
        return 1;
    }
    close(fd2);
    printf("cfidhold: ok\r\n");
    return 0;
}
