/* Internal Ouroboros syscall + IPC layer for the libc (not a standard header).
 *
 * ABI: syscall number in x8, args in x0.., return in x0 (docs/architecture.md).
 * File I/O talks to the filesystem server (fsd) over the uniform ninep verb set
 * via MSG_CALL; console output goes to the console server (cond) the same way,
 * or to a pipe consumer via MSG_SEND. */
#ifndef OUROBOROS_SYS_H
#define OUROBOROS_SYS_H
#include <stddef.h>

/* Syscall numbers (syscall-abi). */
#define SYS_PUTC 4
#define SYS_GET_TICKS 6
#define SYS_READ_CHAR 15
#define SYS_EXIT 17
#define SYS_MSG_SEND 23
#define SYS_MSG_CALL 29
#define SYS_GRANT 31
#define SYS_STDOUT_TARGET 38
#define SYS_HEAP_INFO 40
#define SYS_GET_CWD 51
#define SYS_YIELD 57

/* Transient MSG_SEND failures worth retrying (mirrors syscall-abi). */
#define MSG_ERR_FULL (~0UL - 20UL)
#define MSG_ERR_DENIED (~0UL - 28UL)

/* Task ids + grant modes. */
#define FSD_TASK 2
#define CON_TASK 3
#define GRANT_READ 1

#define HEAP_INFO_BASE 0
#define HEAP_INFO_SIZE 1

/* ninep verbs (ninep-abi; NP_BASE = 0x100). */
#define NP_BASE 0x100
#define NP_WRITE_AT (NP_BASE + 4)
#define NP_TOUCH (NP_BASE + 5)
#define NP_READ_AT (NP_BASE + 10)
#define NP_WRITE_FILE (NP_BASE + 11)
#define NP_STAT (NP_BASE + 12)
/* fids: server-side open-file handles. */
#define NP_OPEN (NP_BASE + 15)
#define NP_PREAD (NP_BASE + 16)
#define NP_PWRITE (NP_BASE + 17)
#define NP_FSTAT (NP_BASE + 18)
#define NP_CLUNK (NP_BASE + 19)
#define OPEN_READ 1
#define OPEN_WRITE 2
#define OPEN_CREATE 4
#define OPEN_TRUNC 8
#define FID_BASE 3

/* Message/payload limits. */
#define NP_REQ_PAYLOAD 48u
#define FS_DATA_MAX 512u
#define MSG_MAX_LEN 768u
#define STAT_INFO_LEN 27u
#define STAT_SIZE_OFF 0u
#define STAT_MODE_OFF 20u

/* Floor of the reserved error band: any syscall/fs return >= this is an error
 * (mirrors syscall-abi's FS_ERR_MIN = u64::MAX - 33). */
#define FS_ERR_MIN (~0UL - 33UL)

static inline long __os_syscall1(long num, long a0) {
    register long x8 asm("x8") = num;
    register long x0 asm("x0") = a0;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return x0;
}

static inline long __os_syscall4(long num, long a0, long a1, long a2, long a3) {
    register long x8 asm("x8") = num;
    register long x0 asm("x0") = a0;
    register long x1 asm("x1") = a1;
    register long x2 asm("x2") = a2;
    register long x3 asm("x3") = a3;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3) : "memory");
    return x0;
}

/* Read a little-endian u64 out of a byte buffer (reply fields). */
static inline unsigned long __rd_u64(const unsigned char *p) {
    unsigned long v = 0;
    for (int i = 0; i < 8; i++) {
        v |= (unsigned long)p[i] << (i * 8);
    }
    return v;
}
static inline void __wr_u64(unsigned char *p, unsigned long v) {
    for (int i = 0; i < 8; i++) {
        p[i] = (unsigned char)(v >> (i * 8));
    }
}

#endif
