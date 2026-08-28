/* Low-level process stubs: task exit and the heap break. The richer I/O stubs
 * (write/read/open/... and stdout routing) live in file.c. */
#include "sys.h"
#include <unistd.h>

void _exit(int code) {
    __os_syscall1(SYS_EXIT, code);
    for (;;) {
    } /* EXIT never returns */
}

/* Grow (or shrink) the program break by `incr`, returning the previous break,
 * or (void*)-1 on failure. The break state lives in .bss statics - the
 * capability the loader's .data/.bss milestone unlocked. The heap region itself
 * is reported once by the kernel (HEAP_INFO). */
void *sbrk(long incr) {
    static char *brk = 0;
    static char *end = 0;
    if (brk == 0) {
        brk = (char *)__os_syscall1(SYS_HEAP_INFO, HEAP_INFO_BASE);
        long size = __os_syscall1(SYS_HEAP_INFO, HEAP_INFO_SIZE);
        if (brk == 0) {
            return (void *)-1; /* no heap for this task */
        }
        end = brk + size;
    }
    if (incr < 0 || brk + incr > end) {
        return (void *)-1;
    }
    char *old = brk;
    brk += incr;
    return old;
}
