/* unlink and rename in the C port, through picolibc (`remove` calls `unlink`).
 * The checks the two handoff notes name: Proem's (a file made, `remove` gives
 * 0, then -1 with ENOENT) and Edit's (`rename` of a temp file over an existing
 * one gives 0, the target holds the temp's text, the temp is gone), and a
 * rename of a missing file (ENOENT). With a different filesystem bound at
 * /mnt/f (`mount 1 /mnt/f`), a rename into it must answer EXDEV; without one
 * that line is only reported. Expects /crename.d/sub to exist (the rig makes
 * it). Build: `make crename-bin`; runs as /bin/CRENAME;
 * `scripts/test-crename.py` drives it on both images. Prints `crename: ok` or
 * `crename: FAIL`. */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("crename: %s %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

static int put(const char *path, const char *text) {
    FILE *f = fopen(path, "w");
    if (f == NULL) {
        return -1;
    }
    int r = fputs(text, f) < 0 ? -1 : 0;
    return fclose(f) != 0 ? -1 : r;
}

/* Whether `path` opens for reading (closing it again: a FILE left open holds
 * one of the port's eight fd slots). */
static int exists(const char *path) {
    FILE *f = fopen(path, "r");
    if (f == NULL) {
        return 0;
    }
    fclose(f);
    return 1;
}

static int holds(const char *path, const char *text) {
    char buf[32];
    FILE *f = fopen(path, "r");
    if (f == NULL) {
        return 0;
    }
    size_t n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    return strcmp(buf, text) == 0;
}

int main(void) {
    check("made the temp and the target", put("/crename.tmp", "new\n") == 0 && put("/crename.txt", "old\n") == 0);
    check("rename over an existing file gives 0", rename("/crename.tmp", "/crename.txt") == 0);
    check("the target holds the temp's text", holds("/crename.txt", "new\n"));
    check("the temp is gone", !exists("/crename.tmp"));

    errno = 0;
    check("rename of a missing file gives -1, ENOENT", rename("/crename.none", "/crename.x") == -1 && errno == ENOENT);

    check("remove gives 0", remove("/crename.txt") == 0);
    check("the file is gone", !exists("/crename.txt"));
    errno = 0;
    check("a second remove gives -1, ENOENT", remove("/crename.txt") == -1 && errno == ENOENT);

    errno = 0;
    check("an empty path gives -1, ENOENT", remove("") == -1 && errno == ENOENT);

    /* `..` collapses before the namespace sees the path, so it can climb out
     * of a mount: /mnt/f/../../x is /x. Uncollapsed, it goes to /mnt/f's tree
     * as /../../x (the ext2 boot binds one there) or to a /mnt that does not
     * exist (the FAT32 boot); fsd's own `..` entries cannot rescue either. */
    check("made a file to reach through ..", put("/crename.dot", "dot\n") == 0);
    check("rename through /mnt/f/../../ gives 0", rename("/mnt/f/../../crename.dot", "/crename.dot2") == 0);
    check("remove through /mnt/f/../.. gives 0", remove("/mnt/f/../../crename.dot2") == 0 && !exists("/crename.dot2"));

    /* A directory moved inside itself is refused, not orphaned. */
    /* /crename.d/sub is made by the rig with the shell's `mkdir` (the port
     * has no mkdir yet). */
    errno = 0;
    check("rename of a directory into itself gives -1, EINVAL", rename("/crename.d", "/crename.d/sub/d") == -1 && errno == EINVAL);
    check("the directory is still there", rmdir("/crename.d/sub") == 0);
    check("remove of an empty directory gives 0", remove("/crename.d") == 0);

    /* Across mounts: only meaningful with /mnt/f bound to another tree. */
    if (put("/crename.x", "x\n") == 0) {
        errno = 0;
        int r = rename("/crename.x", "/mnt/f/crename.x");
        printf("crename: across mounts gives %d, errno %d (%s)\n", r, errno, errno == EXDEV ? "EXDEV" : "not EXDEV");
        if (r == 0) {
            remove("/mnt/f/crename.x");
        } else {
            remove("/crename.x");
        }
    }

    printf("crename: %s\n", failed ? "FAIL" : "ok");
    return failed;
}
