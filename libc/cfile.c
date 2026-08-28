/* C file I/O demo: create + write a file, stat it, read it back - all via the
 * libc's open/write/read/close/fstat over fsd. Output goes through printf (the
 * stdout-target-aware write), so `cfile | grep hello` works too. Built by
 * `make cfile-bin`, staged as /bin/CFILE. */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    const char *path = "/CTEST.TXT";
    const char *text = "hello from C file I/O\r\nline two, written by a C program\r\n";

    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC);
    if (fd < 0) {
        printf("cfile: open (write) failed\r\n");
        return 1;
    }
    write(fd, text, strlen(text));
    close(fd);

    fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("cfile: open (read) failed\r\n");
        return 1;
    }
    struct stat st;
    fstat(fd, &st);
    printf("wrote and re-opened %s (%d bytes):\r\n", path, (int)st.st_size);

    char buf[256];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n > 0) {
        buf[n] = 0;
        printf("%s", buf);
    }
    close(fd);
    return 0;
}
