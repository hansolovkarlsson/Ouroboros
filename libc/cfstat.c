/* fstat through picolibc: every field fstat does not fill comes back zero.
 * Proem knows a file by st_dev and st_ino, and with leftover stack values two
 * headers could compare as one (its fstat-identity handoff); the fallback it
 * accepted is a zero pair, meaning "no identity". This fills a struct stat
 * with 0xA5 bytes, calls fstat on /etc/passwd, and requires the struct to
 * equal one built from zero plus the four fields fstat fills (size, mode, uid,
 * gid), so no other byte, padding included, kept a leftover. It also requires
 * S_ISREG. Prints the mode, uid and gid, which the rig compares per format
 * (ext2's are the disk's, which the rig reads with `ls -l`; FAT32 records
 * none, so 0100666 and 0:0), and checks S_ISDIR on /etc if it opens. Build:
 * `make cfstat-bin`; runs as /bin/CFSTAT; `scripts/test-crename.py` drives it.
 * Prints `cfstat: ok` or `cfstat: FAIL`. */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    int fd = open("/etc/passwd", O_RDONLY);
    if (fd < 0) {
        printf("cfstat: open failed\n");
        printf("cfstat: FAIL\n");
        return 1;
    }
    struct stat st;
    memset(&st, 0xA5, sizeof st);
    int r = fstat(fd, &st);
    close(fd);
    struct stat want;
    memset(&want, 0, sizeof want);
    want.st_size = st.st_size;
    want.st_mode = st.st_mode;
    want.st_uid = st.st_uid;
    want.st_gid = st.st_gid;
    int zeroed = memcmp(&st, &want, sizeof st) == 0;
    printf("cfstat: fstat %d, size %ld, mode %o, uid %u, gid %u, dev %lu, ino %lu\n", r, (long)st.st_size,
           (unsigned)st.st_mode, (unsigned)st.st_uid, (unsigned)st.st_gid, (unsigned long)st.st_dev,
           (unsigned long)st.st_ino);
    printf("cfstat: every other field zero: %s; S_ISREG: %s\n", zeroed ? "yes" : "NO", S_ISREG(st.st_mode) ? "yes" : "NO");
    /* A directory, where NP_OPEN allows one: the type comes from a different
     * part of the record (the flags word) on a filesystem without modes. */
    int dfd = open("/etc", O_RDONLY);
    int dir_ok = 1;
    if (dfd < 0) {
        printf("cfstat: a directory does not open, not checked\n");
    } else {
        struct stat ds;
        memset(&ds, 0xA5, sizeof ds);
        int dr = fstat(dfd, &ds);
        close(dfd);
        dir_ok = dr == 0 && S_ISDIR(ds.st_mode);
        printf("cfstat: /etc mode %o, S_ISDIR: %s\n", (unsigned)ds.st_mode, S_ISDIR(ds.st_mode) ? "yes" : "NO");
    }
    int ok = r == 0 && zeroed && S_ISREG(st.st_mode) && st.st_size > 0 && dir_ok;
    printf("cfstat: %s\n", ok ? "ok" : "FAIL");
    return ok ? 0 : 1;
}
