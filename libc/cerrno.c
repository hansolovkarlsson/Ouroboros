/* errno from the file layer: every failure a C program can provoke there,
 * each checked for the errno POSIX names. Step 2 of
 * docs/roadmap/roadmap-c-hosting.md: until it, only unlink, rmdir, rename and
 * remove set errno, and Proem's include search needs it from open and fopen,
 * where ENOENT or ENOTDIR means "try the next directory" and anything else
 * stops the search.
 *
 * Run as root with no argument for the checks that hold on any filesystem.
 * With a path, it checks only that opening it for reading fails with EACCES:
 * scripts/test-cerrno.py runs `cerrno /etc/shadow` as an ordinary user on
 * ext2, the one image whose modes fsd enforces.
 *
 * One line per check, `cerrno: ok <what>: <errno name>` or `cerrno: FAIL
 * <what>: got <name>, want <name>`, then a summary line; exit 1 on any
 * failure. Runs as /bin/CERRNO, through picolibc. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int g_checks;
static int g_failed;

static const char *name(int e) {
    switch (e) {
    case 0: return "0";
    case ENOENT: return "ENOENT";
    case ENOTDIR: return "ENOTDIR";
    case EISDIR: return "EISDIR";
    case EACCES: return "EACCES";
    case EBADF: return "EBADF";
    case EMFILE: return "EMFILE";
    case EINVAL: return "EINVAL";
    case ENAMETOOLONG: return "ENAMETOOLONG";
    case EFAULT: return "EFAULT";
    case EIO: return "EIO";
    case ENOSYS: return "ENOSYS";
    case ESPIPE: return "ESPIPE";
    case EOVERFLOW: return "EOVERFLOW";
    case ENODEV: return "ENODEV";
    default: return "another";
    }
}

/* `failed` is whether the call returned its failure value; errno is read as
 * the call left it, having been set to 0 before it. A call that succeeds
 * where it should fail is a failed check whatever errno says. */
static void expect(const char *what, int failed, int want) {
    int got = errno;
    g_checks++;
    if (failed && got == want) {
        printf("cerrno: ok %s: %s (%s)\r\n", what, name(got), strerror(got));
        return;
    }
    g_failed++;
    if (!failed) {
        printf("cerrno: FAIL %s: succeeded, want %s\r\n", what, name(want));
    } else {
        printf("cerrno: FAIL %s: got %s, want %s\r\n", what, name(got), name(want));
    }
}

/* A check that is not about errno. */
static void check(const char *what, int ok) {
    g_checks++;
    if (ok) {
        printf("cerrno: ok %s\r\n", what);
        return;
    }
    g_failed++;
    printf("cerrno: FAIL %s\r\n", what);
}

#define TRY(what, call, fails_when, want) \
    do {                                  \
        errno = 0;                        \
        long r_ = (long)(call);           \
        expect(what, (fails_when), want); \
        (void)r_;                         \
    } while (0)

static void as_root(void) {
    char buf[16];
    struct stat st;

    /* open: the cases Proem's include search reads. */
    TRY("open of a missing file", open("/NOSUCH.TXT", O_RDONLY), r_ < 0, ENOENT);
    TRY("open in a missing directory", open("/NODIR/X.H", O_RDONLY), r_ < 0, ENOENT);
    TRY("open through a file as a directory", open("/etc/passwd/X.H", O_RDONLY), r_ < 0, ENOTDIR);
    TRY("open O_WRONLY|O_TRUNC of a missing file", open("/NOSUCH.TXT", O_WRONLY | O_TRUNC), r_ < 0, ENOENT);
    TRY("open of an empty path", open("", O_RDONLY), r_ < 0, ENOENT);
    static char longpath[200];
    memset(longpath, 'A', sizeof longpath - 1);
    longpath[0] = '/';
    TRY("open of a path longer than the library holds", open(longpath, O_RDONLY), r_ < 0, ENAMETOOLONG);
    errno = 0;
    FILE *fp = fopen("/NOSUCH.TXT", "r");
    expect("fopen of a missing file", fp == NULL, ENOENT);
    if (fp) {
        fclose(fp);
    }

    /* A full fd table: the library's own limit, not a server's. */
    int fds[16];
    int n = 0;
    errno = 0;
    while (n < 16) {
        fds[n] = open("/etc/passwd", O_RDONLY);
        if (fds[n] < 0) {
            break;
        }
        n++;
    }
    expect("open with every fd in use", n < 16, EMFILE);
    for (int i = 0; i < n; i++) {
        close(fds[i]);
    }

    /* A bad fd, for each call that takes one. */
    TRY("read of a bad fd", read(99, buf, sizeof buf), r_ < 0, EBADF);
    TRY("write of a bad fd", write(99, "x", 1), r_ < 0, EBADF);
    TRY("close of a bad fd", close(99), r_ < 0, EBADF);
    TRY("fstat of a bad fd", fstat(99, &st), r_ < 0, EBADF);
    TRY("lseek of a bad fd", lseek(99, 0, SEEK_SET), r_ < 0, EBADF);
    int fd = open("/etc/passwd", O_RDONLY);
    if (fd >= 0) {
        close(fd);
        TRY("close of an fd already closed", close(fd), r_ < 0, EBADF);
    }

    /* An fd used against the mode it was opened in. Each open is checked
     * first: on a failed one the call below would get EBADF for fd -1 and
     * pass without reaching the server (the review of #220). */
    fd = open("/etc/passwd", O_RDONLY);
    check("open O_RDONLY for the mode check", fd >= 0);
    TRY("write to an O_RDONLY fd", write(fd, "x", 1), r_ < 0, EBADF);
    TRY("lseek with an unknown whence", lseek(fd, 0, 7), r_ < 0, EINVAL);
    TRY("lseek before the start", lseek(fd, -1, SEEK_SET), r_ < 0, EINVAL);
    TRY("lseek past LONG_MAX", (lseek(fd, 1, SEEK_SET), lseek(fd, LONG_MAX, SEEK_CUR)), r_ < 0, EOVERFLOW);
    lseek(fd, 0, SEEK_SET);
    TRY("lseek of a console fd", lseek(1, 0, SEEK_CUR), r_ < 0, ESPIPE);
    check("lseek's refusals left the offset at 0", lseek(fd, 0, SEEK_CUR) == 0);
    TRY("fstat into a null struct", fstat(fd, NULL), r_ < 0, EFAULT);
    close(fd);
    fd = open("/CERRNO.TMP", O_WRONLY | O_CREAT | O_TRUNC);
    check("open O_WRONLY|O_CREAT for the mode check", fd >= 0);
    TRY("read from an O_WRONLY fd", read(fd, buf, sizeof buf), r_ < 0, EBADF);
    close(fd);
    unlink("/CERRNO.TMP");

    /* The console fds are valid descriptors with no file behind them. */
    check("fstat of a console fd: a character device", fstat(1, &st) == 0 && S_ISCHR(st.st_mode));
    check("close of a console fd succeeds", close(0) == 0);

    /* A directory opened for reading, then read. */
    fd = open("/etc", O_RDONLY);
    if (fd < 0) {
        check("open of a directory for reading", 0);
    } else {
        TRY("read of a directory", read(fd, buf, sizeof buf), r_ < 0, EISDIR);
        close(fd);
    }
}

int main(int argc, char **argv) {
    if (argc > 1) {
        TRY("open of a file the user may not read", open(argv[1], O_RDONLY), r_ < 0, EACCES);
    } else {
        as_root();
    }
    printf("cerrno: %d checks, %d failed\r\n", g_checks, g_failed);
    return g_failed ? 1 : 0;
}
