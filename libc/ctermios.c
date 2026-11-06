/* Terminal settings in a C program: tcgetattr and tcsetattr (sys/termios.h,
 * libc/src/file.c), step 2 of docs/roadmap/roadmap-ctrl-c.md, for item 4 of
 * docs/handoffs/closed/2026-10-05-from-edit-editor-console.md. Runs as /bin/CTERMIOS,
 * through picolibc, driven by scripts/test-ctermios.py.
 *
 * `ctermios check` prints one line per question:
 *
 *     ctermios: initial ok             (ISIG set, OPOST and ONLCR set, VMIN 1,
 *                                       VINTR 3, VQUIT 28)
 *     ctermios: set ok                 (Edit's raw recipe, TCSADRAIN)
 *     ctermios: readback ok            (Edit's recipe reads back as set, but
 *                                       for OPOST: the console always has it)
 *     ctermios: honest ok              (flags the console cannot apply are
 *                                       accepted, and tcgetattr says they are off)
 *     ctermios: restore ok             (the saved settings, TCSAFLUSH)
 *     ctermios: kernel ok              (ISIG follows KBD_MODE set directly)
 *     ctermios: fd 9 EBADF
 *     ctermios: file ENOTTY            (an open file is not a terminal)
 *     ctermios: action EINVAL          (an unknown optional_actions)
 *     ctermios: null EFAULT
 *     ctermios: check done
 *
 * with `... FAIL <what>` in place of an `ok` line that does not hold.
 *
 * `ctermios raw` turns raw mode on with Edit's own recipe (its plat_raw_on),
 * prints `ctermios: raw`, then `ctermios: byte <n>` for each byte read until
 * `q`; then restores the saved settings, prints `ctermios: restored`, and
 * reads on, cooked, until `q`, so a Ctrl+C there ends it.
 *
 * `ctermios flush` and `ctermios drain` turn raw mode on, print
 * `ctermios: spinning`, run two seconds without reading (keys typed then
 * wait in the kernel's queue), and restore the saved settings with
 * TCSAFLUSH, which discards those keys, or TCSADRAIN, which keeps them for
 * the shell; then print `ctermios: flushed` or `ctermios: drained`.
 *
 * `ctermios nl <chunk>` writes five lines, each ending a different way, for the
 * console's ONLCR (the kernel's console::write_bytes): `nl-a` with its `\r`
 * and `\n` in two writes, `nl-b` with a bare `\n`, `nl-c` with `\r\n` in
 * one write, `nl-d` by PUTC a byte at a time with a bare `\n`, and `nl-e`
 * with its `\r` by PUTC between two writes through the console server.
 * Then `nl-f` and `nl-g`, `<chunk>` and `<chunk> - 1` bytes before a bare
 * `\n`, where `<chunk>` is the kernel's (console.rs's `CHUNK`, which the rig
 * reads and passes): the inserted `\r` falls just past the chunk, then at
 * its last byte (write_out). Each must reach the serial line as
 * `nl-X...\r\n`, with no `\r\r`. Every
 * piece is written to fd 2, which goes to the console at once: fd 1 holds
 * its bytes in libc's line buffer until a `\n`, fflush included, so a `\r`
 * written there would leave in the same write as its `\n`. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
#include "include/sys.h"
#include "probe_errno.h"


/* Edit's plat_raw_on, flag for flag (DevTools edit/src/platform_posix.c). */
static void edit_raw(struct termios *t) {
    t->c_iflag &= ~(tcflag_t)(IXON | ICRNL | BRKINT | INPCK | ISTRIP);
    t->c_oflag &= ~(tcflag_t)OPOST;
    t->c_cflag |= CS8;
    t->c_lflag &= ~(tcflag_t)(ECHO | ICANON | ISIG | IEXTEN);
    t->c_cc[VMIN] = 1;
    t->c_cc[VTIME] = 0;
}

static int same(const struct termios *a, const struct termios *b) {
    return a->c_iflag == b->c_iflag && a->c_oflag == b->c_oflag && a->c_cflag == b->c_cflag
        && a->c_lflag == b->c_lflag && memcmp(a->c_cc, b->c_cc, NCCS) == 0;
}

static void result(const char *label, int ok) {
    printf("ctermios: %s %s\r\n", label, ok ? "ok" : "FAIL");
}

static void expect_err(const char *label, int rc) {
    printf("ctermios: %s %s\r\n", label, rc == -1 ? err_name(errno) : "succeeded");
}

static int check(void) {
    struct termios saved, t, back;
    if (tcgetattr(0, &saved) < 0) {
        printf("ctermios: initial FAIL (tcgetattr %s)\r\n", err_name(errno));
        printf("ctermios: check done\r\n");
        return 1;
    }
    result("initial", (saved.c_lflag & ISIG) && (saved.c_oflag & (OPOST | ONLCR)) == (OPOST | ONLCR)
                          && saved.c_cc[VMIN] == 1 && saved.c_cc[VINTR] == 3 && saved.c_cc[VQUIT] == 28);
    t = saved;
    edit_raw(&t);
    /* What is in force after Edit's recipe: the recipe, with OPOST still
     * set, because the console translates a bare \n whatever the settings
     * say. */
    struct termios want = t;
    want.c_oflag |= OPOST | ONLCR;
    result("set", tcsetattr(0, TCSADRAIN, &t) == 0);
    result("readback", tcgetattr(0, &back) == 0 && same(&back, &want));
    /* Flags the console cannot apply: accepted (POSIX: success when any
     * change was made), and tcgetattr shows what is in force, which is the
     * raw settings above, not these. */
    struct termios odd = t;
    odd.c_iflag |= ICRNL | IXON;
    odd.c_oflag |= OCRNL;
    odd.c_lflag |= ECHO | ICANON;
    odd.c_cc[VTIME] = 5;
    result("honest", tcsetattr(0, TCSANOW, &odd) == 0 && tcgetattr(0, &back) == 0 && same(&back, &want));
    result("restore", tcsetattr(0, TCSAFLUSH, &saved) == 0 && tcgetattr(0, &back) == 0
                          && same(&back, &saved));
    /* ISIG is the kernel's answer, not the copy last set: the mode changed
     * behind termios's back shows in tcgetattr. */
    int raw_seen = __os_syscall1(SYS_KBD_MODE, KBD_RAW) == KBD_RAW && tcgetattr(0, &back) == 0
                && !(back.c_lflag & ISIG);
    int cooked_seen = __os_syscall1(SYS_KBD_MODE, KBD_COOKED) == KBD_COOKED && tcgetattr(0, &back) == 0
                   && (back.c_lflag & ISIG);
    result("kernel", raw_seen && cooked_seen);
    errno = 0;
    expect_err("fd 9", tcgetattr(9, &t));
    int fd = open("/include/stdio.h", O_RDONLY);
    if (fd < 0) {
        printf("ctermios: file FAIL (could not open /include/stdio.h)\r\n");
    } else {
        errno = 0;
        expect_err("file", tcgetattr(fd, &t));
        close(fd);
    }
    errno = 0;
    expect_err("action", tcsetattr(0, 7, &saved));
    errno = 0;
    expect_err("null", tcgetattr(0, NULL));
    printf("ctermios: check done\r\n");
    return 0;
}

static void read_until_q(void) {
    unsigned char c;
    while (read(0, &c, 1) == 1 && c != 'q') {
        printf("ctermios: byte %u\r\n", c);
        fflush(stdout);
    }
}

static int raw(void) {
    struct termios saved, t;
    if (tcgetattr(0, &saved) < 0) {
        printf("ctermios: tcgetattr %s\r\n", err_name(errno));
        return 1;
    }
    t = saved;
    edit_raw(&t);
    if (tcsetattr(0, TCSADRAIN, &t) < 0) {
        printf("ctermios: tcsetattr %s\r\n", err_name(errno));
        return 1;
    }
    printf("ctermios: raw\r\n");
    fflush(stdout);
    read_until_q();
    tcsetattr(0, TCSAFLUSH, &saved);
    printf("ctermios: restored\r\n");
    fflush(stdout);
    read_until_q();
    printf("ctermios: bye\r\n");
    return 0;
}

static int spin_then(int action, const char *done) {
    struct termios saved, t;
    if (tcgetattr(0, &saved) < 0) {
        printf("ctermios: tcgetattr %s\r\n", err_name(errno));
        return 1;
    }
    t = saved;
    edit_raw(&t);
    if (tcsetattr(0, TCSADRAIN, &t) < 0) {
        printf("ctermios: tcsetattr %s\r\n", err_name(errno));
        return 1;
    }
    printf("ctermios: spinning\r\n");
    fflush(stdout);
    long start = __os_syscall1(SYS_GET_TICKS, 0);
    while (__os_syscall1(SYS_GET_TICKS, 0) - start < 100) {
    }
    if (tcsetattr(0, action, &saved) < 0) {
        printf("ctermios: restore %s\r\n", err_name(errno));
        return 1;
    }
    printf("ctermios: %s\r\n", done);
    return 0;
}

static void putc_text(const char *s) {
    for (; *s; s++) {
        __os_syscall1(SYS_PUTC, (unsigned char)*s);
    }
}

/* One console write, now: fd 2 is not buffered (libc/src/file.c's write). */
static void now(const char *s) {
    write(2, s, strlen(s));
}

static int newlines(int chunk) {
    now("nl-a\r");
    now("\n");
    now("nl-b\n");
    now("nl-c\r\n");
    putc_text("nl-d\n");
    now("nl-e");
    putc_text("\r");
    now("\n");
    /* The chunk's two edges, `chunk` and `chunk - 1` bytes before a bare
     * \n: the first puts the inserted \r just past the kernel's chunk, the
     * second at its last byte. Each in one write, so it reaches the kernel's
     * console::write_out whole (libc and cond pass FS_DATA_MAX at a time). */
    static char line[FS_DATA_MAX];
    if (chunk < 5 || chunk + 1 > (int)sizeof line) {
        now("ctermios: nl needs the kernel's chunk size, 5 to 511\r\n");
        return 2;
    }
    for (int n = chunk; n >= chunk - 1; n--) {
        memset(line, 'x', sizeof line);
        memcpy(line, n == chunk ? "nl-f" : "nl-g", 4);
        line[n] = '\n';
        write(2, line, (size_t)n + 1);
    }
    now("ctermios: nl done\r\n");
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "flush") == 0) {
        return spin_then(TCSAFLUSH, "flushed");
    }
    if (argc > 1 && strcmp(argv[1], "drain") == 0) {
        return spin_then(TCSADRAIN, "drained");
    }
    if (argc > 1 && strcmp(argv[1], "raw") == 0) {
        return raw();
    }
    if (argc > 1 && strcmp(argv[1], "check") == 0) {
        return check();
    }
    if (argc > 1 && strcmp(argv[1], "nl") == 0) {
        return newlines(argc > 2 ? atoi(argv[2]) : 0);
    }
    printf("usage: ctermios check|raw|flush|drain|nl <chunk>\r\n");
    return 2;
}
