/* Open a path with O_RDONLY|O_CREAT and read from it: the check that fsd's
 * read-only gate refuses `O_CREAT` only when it would create (POSIX: EROFS
 * "if the file does not exist"), so an existing file on a read-only volume
 * opens and reads (`mutates` and `NP_OPEN` in programs/servers/fsd/src/main.rs,
 * the high review of #364). Built by `make ccreat-bin`, staged as /bin/CCREAT;
 * `scripts/test-long-set.py` drives it on its read-only exFAT boots.
 *
 * `ccreat PATH` prints `ccreat: PATH read N` (the bytes one read of up to 64
 * returned) or `ccreat: PATH errno E` for the open or the read that failed. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char buf[64];
    if (argc != 2) {
        printf("usage: ccreat PATH\n");
        return 2;
    }
    int fd = open(argv[1], O_RDONLY | O_CREAT, 0644);
    if (fd < 0) {
        printf("ccreat: %s errno %d\n", argv[1], errno);
        return 1;
    }
    long n = read(fd, buf, sizeof buf);
    if (n < 0) {
        printf("ccreat: %s errno %d\n", argv[1], errno);
        close(fd);
        return 1;
    }
    printf("ccreat: %s read %d\n", argv[1], (int)n);
    close(fd);
    return 0;
}
