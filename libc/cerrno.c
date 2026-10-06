/* errno from the file layer: every failure a C program can provoke there,
 * each checked for the errno POSIX names. Step 2 of
 * docs/roadmap/roadmap-c-hosting.md: until it, only unlink, rmdir, rename and
 * remove set errno, and Proem's include search needs it from open and fopen,
 * where ENOENT or ENOTDIR means "try the next directory" and anything else
 * stops the search.
 *
 * Run as root with no argument for the checks that hold on any filesystem,
 * stat(path)'s among them since step 4; three of those need `mount -c
 * /dev/cons` and `mount -n /net` made first. With a path, it checks only that
 * opening it for reading fails with EACCES and that stat of it succeeds (no
 * read permission is needed for that), then each further `<path> <octal
 * mode> <uid> <gid>` group against stat: scripts/test-cerrno.py runs it so
 * as an ordinary user on ext2, the one image whose modes fsd enforces.
 *
 * One line per check, `cerrno: ok <what>: <errno name>` or `cerrno: FAIL
 * <what>: got <name>, want <name>`, then a summary line; exit 1 on any
 * failure. Runs as /bin/CERRNO, through picolibc. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
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

    /* stat(path), step 4: the answers open gives for a path that is not
     * there, and fstat's record for one that is. */
    TRY("stat of a missing file", stat("/NOSUCH.TXT", &st), r_ < 0, ENOENT);
    TRY("stat through a file as a directory", stat("/etc/passwd/X.H", &st), r_ < 0, ENOTDIR);
    TRY("stat of an empty path", stat("", &st), r_ < 0, ENOENT);
    TRY("stat into a null struct", stat("/etc/passwd", NULL), r_ < 0, EFAULT);
    check("stat of a directory: a directory", stat("/etc", &st) == 0 && S_ISDIR(st.st_mode));
    {
        struct stat byfd;
        int sfd = open("/etc/passwd", O_RDONLY);
        int ok = sfd >= 0 && fstat(sfd, &byfd) == 0 && stat("/etc/passwd", &st) == 0;
        check("stat of a file: a regular file, the record fstat reads",
              ok && S_ISREG(st.st_mode) && st.st_size > 0 && st.st_size == byfd.st_size &&
                  st.st_mode == byfd.st_mode && st.st_uid == byfd.st_uid &&
                  st.st_gid == byfd.st_gid);
        if (sfd >= 0) {
            close(sfd);
        }
        struct stat viol;
        check("lstat of a file: what stat says (no symbolic links here)",
              lstat("/etc/passwd", &viol) == 0 && viol.st_mode == st.st_mode &&
                  viol.st_size == st.st_size);
    }
    /* A trailing `/` or `/.` names a directory (POSIX), though the library
     * collapses the path as text before the server sees it. */
    check("stat of a directory with a trailing slash", stat("/etc/", &st) == 0 && S_ISDIR(st.st_mode));
    TRY("stat of a file with a trailing slash", stat("/etc/passwd/", &st), r_ < 0, ENOTDIR);
    TRY("stat of a file with a trailing /.", stat("/etc/passwd/.", &st), r_ < 0, ENOTDIR);
    /* The console and /net bindings. These need scripts/test-cerrno.py's
     * `mount -c /dev/cons` and `mount -n /net` first. */
    check("stat of the console binding: a character device",
          stat("/dev/cons", &st) == 0 && S_ISCHR(st.st_mode));
    TRY("stat of a path below the console binding", stat("/dev/cons/NOSUCH", &st), r_ < 0, ENOENT);
    /* netd answers NP_STAT wrongly under /net/tcp, so stat answers no /net
     * path yet, as open does not (on the roadmap). */
    TRY("stat of a /net path, not answered yet", stat("/net/ip", &st), r_ < 0, ENOSYS);

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
    /* Whether the table really filled, read before expect() reports it: a
     * first open failing for another reason also leaves n < 16 (the review
     * of #223). */
    int full = n < 16 && errno == EMFILE;
    expect("open with every fd in use", n < 16, EMFILE);
    /* stat takes no fd, so a full table does not stop it. */
    check("stat with every fd in use", full && stat("/etc/passwd", &st) == 0);
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
        /* POSIX's stat needs no read permission on the file, only search on
         * the directories above it, which is fsd's rule for NP_STAT. */
        struct stat st;
        check("stat of that file succeeds, a regular file",
              stat(argv[1], &st) == 0 && S_ISREG(st.st_mode));
        /* Then `<path> <octal mode> <uid> <gid>` groups: values the caller
         * knows independently of this library (the image's build sets them),
         * so a decoding mistake shared by stat and fstat is caught here,
         * where comparing the two could not (the review of #223). */
        for (int i = 2; i + 3 < argc; i += 4) {
            unsigned mode = (unsigned)strtoul(argv[i + 1], NULL, 8);
            unsigned uid = (unsigned)strtoul(argv[i + 2], NULL, 10);
            unsigned gid = (unsigned)strtoul(argv[i + 3], NULL, 10);
            errno = 0;
            if (stat(argv[i], &st) != 0) {
                /* The values in `st` are the previous file's: say why. */
                printf("cerrno: stat %s failed: %s\r\n", argv[i], name(errno));
                check("stat of a file: the mode and owner the image gave it", 0);
                continue;
            }
            printf("cerrno: stat %s: mode %o uid %u gid %u, want %o %u %u\r\n", argv[i],
                   st.st_mode & 07777, st.st_uid, st.st_gid, mode, uid, gid);
            check("stat of a file: the mode and owner the image gave it",
                  S_ISREG(st.st_mode) && (st.st_mode & 07777) == mode && st.st_uid == uid &&
                      st.st_gid == gid);
        }
    } else {
        as_root();
    }
    printf("cerrno: %d checks, %d failed\r\n", g_checks, g_failed);
    return g_failed ? 1 : 0;
}
