/* The C twin of /bin/ARGS: prints the argument vector crt0 built, in exactly
 * the format the Rust program prints the kernel's, so the two outputs compare
 * byte for byte (`ARGS a b > /r.txt`, `CARGS a b > /c.txt`, then the same text
 * apart from argv[0], the program's own path). Step 1 of
 * docs/roadmap/roadmap-c-hosting.md, and its check: until then crt0 called
 * main(void) and no C program could read its command line.
 *
 * It also prints argv[argc], which must be NULL (C11 5.1.2.2.1), as a line the
 * Rust program has no counterpart for. Runs as /bin/CARGS, through picolibc,
 * the library Proem links. */
#include <stdio.h>

int main(int argc, char **argv) {
    printf("argc=%d\r\n", argc);
    for (int i = 0; i < argc; i++) {
        printf("argv[%d] = %s\r\n", i, argv[i]);
    }
    printf("argv[argc] %s\r\n", argv[argc] == NULL ? "is NULL" : "is NOT NULL");
    return argv[argc] == NULL ? 0 : 1;
}
