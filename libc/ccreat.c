/* Open a path with O_RDONLY|O_CREAT and read from it, or write to it: the
 * check that fsd refuses `O_CREAT` only when it would create (POSIX: EROFS
 * "if the file does not exist"), so an existing file on a read-only volume
 * opens and reads, and that such an fd is read-only, so its write is EBADF
 * (`refused_read_only`, `check_access` and the fid's write check in
 * programs/servers/fsd/src/main.rs, the high and medium reviews of #364).
 * Built by `make ccreat-bin`, staged as /bin/CCREAT; `scripts/test-long-set.py`,
 * `scripts/test-boot-partition.py` and `scripts/test-cerrno.py` drive it.
 *
 * `ccreat PATH` prints `ccreat: PATH read N` (the bytes one read of up to 64
 * returned), and `ccreat -w PATH` writes one byte and prints
 * `ccreat: PATH wrote N`. `ccreat -x PATH` opens O_WRONLY|O_CREAT|O_EXCL,
 * the lockfile idiom, writes one byte and prints `ccreat: PATH made N`; on a
 * name that exists the open must fail EEXIST (libc dropped O_EXCL until
 * 2026-10-28, and the open succeeded). A failure prints `ccreat: PATH open errno E`,
 * `read errno E` or `write errno E`, naming the call, so a file created and
 * then refused cannot pass for one refused at the open (the medium review of
 * #364). */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char buf[64];
    int excl = argc == 3 && strcmp(argv[1], "-x") == 0;
    int writing = excl || (argc == 3 && strcmp(argv[1], "-w") == 0);
    if (argc != 2 && !writing) {
        printf("usage: ccreat [-w | -x] PATH\n");
        return 2;
    }
    const char *path = argv[argc - 1];
    int fd = open(path, excl ? O_WRONLY | O_CREAT | O_EXCL : O_RDONLY | O_CREAT, 0644);
    if (fd < 0) {
        printf("ccreat: %s open errno %d\n", path, errno);
        return 1;
    }
    long n = writing ? write(fd, "x", 1) : read(fd, buf, sizeof buf);
    if (n < 0) {
        printf("ccreat: %s %s errno %d\n", path, writing ? "write" : "read", errno);
        close(fd);
        return 1;
    }
    printf("ccreat: %s %s %d\n", path, excl ? "made" : writing ? "wrote" : "read", (int)n);
    close(fd);
    return 0;
}
