/* The libc's bottom edge: the POSIX-ish "syscall stubs" that translate to
 * Ouroboros's syscall boundary. This is the porting layer a real libc
 * (picolibc/newlib) plugs into; here it backs our own minimal stdio/stdlib.
 *
 * Scope of this first cut: write/read go to the console/keyboard (fd ignored -
 * a stdout-target-aware write for pipes is a follow-up); sbrk manages a break
 * over the kernel-reported heap region; _exit ends the task. File I/O
 * (open/close/lseek/fstat via fsd's NP_* protocol) is a documented next step. */
#include "sys.h"
#include <unistd.h>

void _exit(int code) {
    __os_syscall1(SYS_EXIT, code);
    for (;;) {
    } /* EXIT never returns */
}

ssize_t write(int fd, const void *buf, size_t count) {
    (void)fd; /* all output -> console for now */
    const unsigned char *p = (const unsigned char *)buf;
    for (size_t i = 0; i < count; i++) {
        __os_syscall1(SYS_PUTC, p[i]);
    }
    return (ssize_t)count;
}

ssize_t read(int fd, void *buf, size_t count) {
    (void)fd; /* all input <- keyboard (stdin) */
    if (count == 0) {
        return 0;
    }
    long c = __os_syscall1(SYS_READ_CHAR, 0); /* blocks until a key */
    ((unsigned char *)buf)[0] = (unsigned char)c;
    return 1;
}

/* Grow (or shrink) the program break by `incr`, returning the previous break,
 * or (void*)-1 on failure. The break state lives in .bss statics - which is
 * exactly the capability the loader's .data/.bss milestone unlocked. The heap
 * region itself is reported once by the kernel (HEAP_INFO). */
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
