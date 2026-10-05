/* The C twin of /bin/ARGS: prints the argument vector crt0 built, in exactly
 * the format the Rust program prints the kernel's, so the two outputs compare
 * byte for byte (`args a b` and `cargs a b` print the same text apart from
 * argv[0], each program's name as typed). Step 1 of
 * docs/roadmap/roadmap-c-hosting.md, and its check: until then crt0 called
 * main(void) and no C program could read its command line.
 *
 * It also prints argv[argc], which must be NULL (C11 5.1.2.2.1), as a line the
 * Rust program has no counterpart for. Built twice, because crt0 is linked
 * into both C libraries: /bin/CARGS through picolibc, the library Proem
 * links, and /bin/CARGSH through the hand-rolled libc. */
#include <stddef.h>
#include <stdio.h>

int main(int argc, char **argv) {
    printf("argc=%d\r\n", argc);
    for (int i = 0; i < argc; i++) {
        printf("argv[%d] = %s\r\n", i, argv[i]);
    }
    printf("argv[argc] %s\r\n", argv[argc] == NULL ? "is NULL" : "is NOT NULL");
    return argv[argc] == NULL ? 0 : 1;
}
