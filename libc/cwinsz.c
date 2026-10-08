/* The console's size in a C program: ioctl(TIOCGWINSZ), libc/src/file.c,
 * item 3 of docs/handoffs/2026-10-05-from-edit-editor-console.md.
 *
 * Prints one line per question, for scripts/test-cwinsz.py to compare with
 * what the console really is:
 *
 *     cwinsz: fd 0 rows=<r> cols=<c>
 *     cwinsz: fd 1 rows=<r> cols=<c>      (or `fd 1 ENOTTY` when piped)
 *     cwinsz: fd 2 rows=<r> cols=<c>
 *     cwinsz: fd 9 EBADF                  (never opened)
 *     cwinsz: file ENOTTY                 (an open file is not a terminal)
 *     cwinsz: request ENOTTY              (any other request)
 *
 * Exit 0. Runs as /bin/CWINSZ, through picolibc. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include "probe_errno.h"


static void ask(const char *label, int fd, unsigned long request) {
    struct winsize ws = {0xffff, 0xffff, 0xffff, 0xffff};
    errno = 0;
    if (ioctl(fd, request, &ws) == 0) {
        printf("cwinsz: %s rows=%u cols=%u\r\n", label, ws.ws_row, ws.ws_col);
    } else {
        printf("cwinsz: %s %s\r\n", label, err_name(errno));
    }
}

int main(void) {
    ask("fd 0", 0, TIOCGWINSZ);
    ask("fd 1", 1, TIOCGWINSZ);
    ask("fd 2", 2, TIOCGWINSZ);
    ask("fd 9", 9, TIOCGWINSZ);
    int fd = open("/include/stdio.h", O_RDONLY);
    if (fd >= 0) {
        ask("file", fd, TIOCGWINSZ);
        close(fd);
    } else {
        printf("cwinsz: file open failed\r\n");
    }
    ask("request", 1, 0x5401); /* TCGETS: no termios here */
    return 0;
}
