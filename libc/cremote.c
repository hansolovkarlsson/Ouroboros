/* Open and read a file on a REMOTE mount, from C - step 3b of
 * docs/roadmap-fid-verbs.md, and the thing that could not be done before.
 *
 * Fixed paths, because C programs get no argv yet (crt0.c calls main() with no
 * arguments). Run it after `mount -r <host>:<port> /mnt/a`.
 *
 * The local path is checked FIRST and deliberately: every existing C program
 * takes that route, so a change that fixed the remote case by breaking the
 * local one would otherwise look like a pass.
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "sys.h"

unsigned long ouro_last_fs_status(void);

static const char *why(void) {
    unsigned long s = ouro_last_fs_status();
    if (s == FS_ERR_NOT_FOUND) return "no such file or directory";
    if (s == FS_ERR_PERM) return "permission denied";
    if (s == FS_ERR_NO_SUCH_VERB) return "that server does not implement this request";
    if (s == MSG_ERR_DENIED) return "not allowed to reach that server (capability)";
    if (s >= FS_ERR_MIN) return "failed";
    return "no error recorded";
}

static int try_read(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("%s: open failed: %s\r\n", path, why());
        return 1;
    }
    struct stat st;
    st.st_size = 0;
    if (fstat(fd, &st) != 0) {
        printf("%s: fstat failed: %s\r\n", path, why());
        close(fd);
        return 1;
    }
    char buf[128];
    ssize_t n = read(fd, buf, sizeof buf - 1);
    if (n < 0) {
        printf("%s: read failed: %s\r\n", path, why());
        close(fd);
        return 1;
    }
    buf[n] = 0;
    /* Trim to the first line, so the check is one readable line of output. */
    for (int i = 0; buf[i]; i++) {
        if (buf[i] == '\n' || buf[i] == '\r') { buf[i] = 0; break; }
    }
    printf("%s: fd=%d size=%d first=%s\r\n", path, fd, (int)st.st_size, buf);
    close(fd);
    return 0;
}

int main(void) {
    int bad = 0;
    bad |= try_read("/EFI/ORBS/INIT.CFG");  /* local - the no-regression check */
    bad |= try_read("/mnt/a/HELLO.TXT");    /* remote - the new capability */
    return bad;
}
