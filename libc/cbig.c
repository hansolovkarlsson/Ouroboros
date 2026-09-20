/* Read a file LARGER than one remote chunk over a remote mount, and compare it
 * byte-for-byte against this machine's own identical copy - the witness for the
 * CLIENT half of step 5 of docs/roadmap/roadmap-fid-verbs.md (Decision 4, the
 * held client session).
 *
 * Why this and not cremote: cremote proves the fid LIFECYCLE (open, fstat,
 * read, close as four separate NETOP_RMOUNT calls that only succeed if netd
 * holds one TCP connection across them). It reads 128 bytes, so one NP_PREAD.
 * This reads a file bigger than NP_REMOTE_CHUNK (512), so a single read() does
 * SEVERAL preads on the one held session - the rising-offset loop the session
 * is FOR, exercised as data rather than argued.
 *
 * Self-checking, no host oracle: /man/grep is staged identically on both nodes,
 * so the LOCAL /man/grep and the REMOTE /mnt/a/man/grep must be the same bytes.
 * If netd closed the connection after each verb (the mutation control), the
 * remote read fails partway and the compare - or the size - differs.
 *
 * Fixed paths, because C programs get no argv yet. Run on node B after
 * `mount -r 10.0.2.10:564 /mnt/a` (or 10.0.2.11 from A).
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "sys.h"

/* Read the whole file at `path` into `buf` (up to `cap`), returning the byte
 * count or -1. One read() per call is enough: libc's read() loops internally,
 * issuing an NP_PREAD per FS_DATA_MAX chunk, so on a file over 512 bytes this
 * is already several preads on the held session. Loops anyway, in case a short
 * read ever splits it. */
static long slurp(const char *path, unsigned char *buf, long cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    long got = 0;
    for (;;) {
        ssize_t n = read(fd, buf + got, (size_t)(cap - got));
        if (n < 0) {
            close(fd);
            return -1;
        }
        if (n == 0 || got + n >= cap) {
            got += n;
            break;
        }
        got += n;
    }
    close(fd);
    return got;
}

int main(void) {
    static unsigned char local[8192];
    static unsigned char remote[8192];

    long ln = slurp("/man/grep", local, sizeof local);
    if (ln < 0) {
        printf("cbig: local /man/grep read failed: %s\r\n", ouro_fs_strerror(ouro_last_fs_status()));
        return 1;
    }
    long rn = slurp("/mnt/a/man/grep", remote, sizeof remote);
    if (rn < 0) {
        printf("cbig: remote /mnt/a/man/grep read failed: %s\r\n", ouro_fs_strerror(ouro_last_fs_status()));
        return 1;
    }
    /* The check that can fail: same length, same bytes, and longer than one
     * chunk (or a single-pread read would pass a broken session). */
    int longer = ln > 512;
    int same = (ln == rn) && (memcmp(local, remote, (size_t)ln) == 0);
    if (same && longer) {
        printf("cbig: %d bytes over the remote mount match the local copy "
               "(%d > one chunk)\r\n", (int)rn, (int)ln);
        return 0;
    }
    printf("cbig: MISMATCH - local %d bytes, remote %d bytes, %s, %s\r\n",
           (int)ln, (int)rn, same ? "equal" : "DIFFER",
           longer ? "longer than a chunk" : "NOT longer than a chunk");
    return 1;
}
