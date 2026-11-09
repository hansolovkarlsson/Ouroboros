/* Hold a remote file open across a logout: the session case of user-keys step
 * 8 (docs/roadmap/roadmap-user-keys.md). A fid on a remote mount keeps netd's
 * client session to that export open, and a session opened under a user's
 * credential must close when that user's last key goes, so that nothing
 * running as the same uid afterwards rides it as the attested user.
 *
 *   exec /bin/cfidwait <remote file> [seconds]
 *
 * Opens the file, reads it, and prints `cfidwait: held <path> first=<line>`;
 * waits `seconds` (default 30) with the fid open; then reads it again and
 * prints `cfidwait: second read ok` or `cfidwait: second read failed (<why>)`,
 * and closes. A session closed under it fails the second read, which is the
 * check's second witness beside the capture's FIN.
 *
 *   exec /bin/cfidwait -n [seconds]
 *
 * The other mode: send netd a message of an op it does not know, yielding
 * between sends, for `seconds` (default 30), then print how many it sent.
 * Run by root while `user` reads a remote file, it fills netd's mailbox with
 * another uid's messages, so that the last message netd received before it
 * signs a parked request is very likely root's and not the request's own:
 * the condition under which a credential signed for "whoever sent last"
 * (`SENDER_ID` read at sign time) differs from one signed for the uid
 * captured at the park. netd answers the op as it answers any unknown one.
 *
 * Built by `make cfidwait-bin`, staged as /bin/CFIDWAIT;
 * `scripts/test-cred-client.py` drives it. */
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include "sys.h"
#include "nsresolve.h"

#define TICKS_PER_S 50  /* the 20 ms tick */

static void pause_ticks(long n) {
    long until = __os_syscall1(SYS_GET_TICKS, 0) + n;
    while (__os_syscall1(SYS_GET_TICKS, 0) < until) {
        __os_syscall1(SYS_YIELD, 0);
    }
}

/* The first line of a read, NUL-terminated in place. */
static void first_line(char *buf, long n) {
    buf[n] = 0;
    for (long i = 0; i < n; i++) {
        if (buf[i] == '\n' || buf[i] == '\r') {
            buf[i] = 0;
            break;
        }
    }
}

/* The `-n` mode: an op no netd arm knows, so it is answered as the
 * supervisor's ping is. */
static int nudge(long secs) {
    static const unsigned char op[8] = {0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f};
    long until = __os_syscall1(SYS_GET_TICKS, 0) + secs * TICKS_PER_S;
    long sent = 0;
    while (__os_syscall1(SYS_GET_TICKS, 0) < until) {
        if (__os_syscall4(SYS_MSG_SEND, NET_TASK, (long)op, sizeof op, 0) == 0) {
            sent++;
        }
        __os_syscall1(SYS_YIELD, 0);
    }
    printf("cfidwait: nudged netd %d times\r\n", (int)sent);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        printf("usage: cfidwait <remote file> [seconds] | cfidwait -n [seconds]\r\n");
        return 2;
    }
    if (argv[1][0] == '-' && argv[1][1] == 'n' && argv[1][2] == 0) {
        return nudge(argc > 2 ? atoi(argv[2]) : 30);
    }
    long secs = argc > 2 ? atoi(argv[2]) : 30;
    char buf[64];
    int fd = open(argv[1], O_RDONLY);
    if (fd < 0) {
        printf("cfidwait: open failed (%s)\r\n", ouro_fs_strerror());
        return 1;
    }
    long n = read(fd, buf, sizeof buf - 1);
    if (n < 0) {
        printf("cfidwait: first read failed (%s)\r\n", ouro_fs_strerror());
        close(fd);
        return 1;
    }
    first_line(buf, n);
    printf("cfidwait: held %s first=%s\r\n", argv[1], buf);
    pause_ticks(secs * TICKS_PER_S);
    if (lseek(fd, 0, 0) < 0 || (n = read(fd, buf, sizeof buf - 1)) < 0) {
        printf("cfidwait: second read failed (%s)\r\n", ouro_fs_strerror());
    } else {
        printf("cfidwait: second read ok\r\n");
    }
    close(fd);
    return 0;
}
