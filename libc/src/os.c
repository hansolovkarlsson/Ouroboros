/* Low-level process stubs: task exit and the heap break. The richer I/O stubs
 * (write/read/open/... and stdout routing) live in file.c. */
#include "sys.h"
#include <unistd.h>

extern void __libc_close_all(void);
/* Weakly declared: our stdio.c defines it, picolibc programs don't link that
 * file at all. A weak undefined symbol resolves to 0, so the call below is
 * skipped when there is no such buffer to drain. */
extern void __libc_flush_stdout(void) __attribute__((weak));

void _exit(int code) {
    /* The one exit path every C library reaches, so everything that must happen
     * before a program disappears happens here - IN ORDER, because two buffers
     * stack and the end-of-stream marker must not overtake the data.
     *
     * Our own stdlib.c's exit() already does the first two, but a picolibc
     * program does not link that stdlib.c (picolibc supplies exit()), so
     * without this a picolibc program in a pipeline left its last line unsent
     * and never signalled end-of-stream. All of these are idempotent. */
    if (__libc_flush_stdout) {
        __libc_flush_stdout(); /* stdio's buffer, which sits above... */
    }
    __libc_end_stdout(); /* ...the write-boundary buffer, then the EOS marker */
    /* Release the server-side fids: nothing else does, and a leaked one is
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
