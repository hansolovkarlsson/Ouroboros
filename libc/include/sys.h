/* Internal Ouroboros syscall layer for the libc (not a standard header).
 *
 * ABI: syscall number in x8, args in x0.., return in x0 (docs/architecture.md).
 * The minimal libc needs only 0/1-argument syscalls. */
#ifndef OUROBOROS_SYS_H
#define OUROBOROS_SYS_H

#define SYS_PUTC 4
#define SYS_READ_CHAR 15
#define SYS_EXIT 17
#define SYS_HEAP_INFO 40
#define HEAP_INFO_BASE 0
#define HEAP_INFO_SIZE 1

static inline long __os_syscall1(long num, long arg0) {
    register long x8 asm("x8") = num;
    register long x0 asm("x0") = arg0;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return x0;
}

#endif
