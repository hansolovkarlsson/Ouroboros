/* The environment in a C program: `environ` as crt0 built it, printed in
 * exactly /bin/PRINTENV's format, so the two compare line for line, and then
 * getenv's answer for each name given as an argument. Step 3 of
 * docs/roadmap/roadmap-c-hosting.md: until it nothing defined `environ`, so a
 * picolibc program calling getenv (Proem reads SOURCE_DATE_EPOCH) did not
 * link.
 *
 * Built twice, as cargs is, because crt0 is linked into both C libraries:
 * /bin/CENV through picolibc, and /bin/CENVH through the hand-rolled libc,
 * which has no getenv, so that build prints `environ` only. Driven by
 * scripts/test-cenv.py. */
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

extern char **environ;

int main(int argc, char **argv) {
    for (char **e = environ; *e != NULL; e++) {
        printf("%s\r\n", *e);
    }
#ifdef __PICOLIBC__
    for (int i = 1; i < argc; i++) {
        const char *v = getenv(argv[i]);
        if (v) {
            printf("getenv %s = [%s]\r\n", argv[i], v);
        } else {
            printf("getenv %s unset\r\n", argv[i]);
        }
    }
#else
    (void)argc;
    (void)argv;
#endif
    return 0;
}
