/* A C program written against the Ouroboros minimal libc - no hand-rolled
 * syscalls, just standard-ish stdio/stdlib/string. Built by `make cdemo-bin`,
 * staged as /bin/CDEMO. Exercises printf, malloc/free, and the string helpers. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    printf("Ouroboros minimal libc\r\n");
    printf("  printf: int=%d uint=%u hex=%x char=%c str=%s\r\n",
           -42, 3000000000u, 0xBEEF, 'Q', "ok");

    char *buf = malloc(64);
    strcpy(buf, "malloc + strcpy, len=");
    printf("  %s%d\r\n", buf, (int)strlen(buf));
    free(buf);

    long sum = 0;
    for (int i = 1; i <= 100; i++) {
        sum += i;
    }
    printf("  sum(1..100) = %d\r\n", (int)sum);

    return 0;
}
