/* Write a file LARGER than one remote chunk THROUGH a fid onto a remote mount,
 * then read it back and compare - the witness for NP_PWRITE (step 7 of
 * docs/roadmap/roadmap-fid-verbs.md, the C remote-write path it unblocks).
 *
 * cbig proved the remote READ path; this proves the remote WRITE path. The
 * pattern is 800 bytes, so libc's write() loops (512 + 288) and the export
 * relays two NP_PWRITE over one held session, then read() loops back the same
 * way - the rising-offset write the fid exists for, exercised as data.
 *
 * Self-checking, no host oracle: it writes a known pattern and compares the
 * read-back against it.
 *
 * Fixed path, because C programs get no argv yet. Run on node B after
 * `mount -r 10.0.2.10:564 /mnt/a`. Run it as ROOT for the positive case (root
 * may write A's root dir); as a normal USER the create is REFUSED (no write on
 * A's root-owned directory), which is step 7's permission control and needs
 * the EXT2 pair - FAT32 records no mode and would let the write through.
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "sys.h"

static const char *why(void) {
    return ouro_fs_strerror(ouro_last_fs_status());
}

int main(void) {
    const char *path = "/mnt/a/CWTEST.TXT";
    static char pat[800];
    for (int i = 0; i < (int)sizeof pat; i++) {
        pat[i] = (char)('A' + (i % 26));
    }

    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC);
    if (fd < 0) {
        printf("cwrite: open (write) refused: %s\r\n", why());
        return 1;
    }
    ssize_t w = write(fd, pat, sizeof pat);
    close(fd);
    if (w != (ssize_t)sizeof pat) {
        printf("cwrite: write returned %d, wanted %d: %s\r\n",
               (int)w, (int)sizeof pat, why());
        return 1;
    }

    static char got[800];
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("cwrite: reopen (read) failed: %s\r\n", why());
        return 1;
    }
    ssize_t r = read(fd, got, sizeof got);
    close(fd);
    if (r != (ssize_t)sizeof pat || memcmp(pat, got, sizeof pat) != 0) {
        printf("cwrite: readback MISMATCH - wrote %d, read %d\r\n",
               (int)sizeof pat, (int)r);
        return 1;
    }
    printf("cwrite: %d bytes written through a remote fid and read back "
           "identical (more than one %d-byte chunk)\r\n",
           (int)sizeof pat, (int)FS_DATA_MAX);
    return 0;
}
