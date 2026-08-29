/* Low-level process stubs: task exit and the heap break. The richer I/O stubs
 * (write/read/open/... and stdout routing) live in file.c. */
#include "sys.h"
#include <unistd.h>

/* Weakly declared: our stdio.c defines it, picolibc programs don't link that
 * file at all. A weak undefined symbol resolves to 0, so the call below is
 * skipped when there is no such buffer to drain. */
extern void __libc_flush_stdout(void) __attribute__((weak));
extern void __libc_close_all(void);

void _exit(int code) {
    /* The one exit path every C library reaches. Our own stdlib.c's exit()
     * already flushes and marks end-of-stream before getting here, but a
     * picolibc program does not link that stdlib.c - picolibc supplies exit(),
     * which comes straight here. So do it here too (both calls are idempotent):
     * without this, a picolibc program in a pipeline would leave its buffered
     * last line unsent and never signal end-of-stream to the consumer. */
    /* TWO buffers stack here: stdio's (if linked) sits above the write-boundary
     * one in file.c. Draining only the lower one meant a program calling _exit()
     * directly - rather than exit() - sent its end-of-stream marker with data
     * still sitting in stdio, and the consumer never saw it. Top down. */
    if (__libc_flush_stdout) {
        __libc_flush_stdout();
    }
    __libc_end_stdout();
    /* Release the server-side fids too: nothing else does, and a leaked one is
     * unreapable once the task's slot is recycled. */
    __libc_close_all();
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
