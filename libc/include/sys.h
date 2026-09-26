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

/* Task-management codes (mirrors syscall-abi's TASK_ERR_*; the list of
 * syscalls that answer them is on the Rust constants, not repeated here).
 * TASK_ERR_NO_SUCH_TASK is the one of the three ouro_last_fs_status() can
 * report: the destination slot held no server this boot, or the server died
 * with this request in flight (its supervisor reinstalls it before this
 * program runs again, so there is nothing to wait for, and whether the dead
 * server had acted on the request is unknowable). Only the fsd/netd request
 * path records a status; the console write falls back to PUTC and the pipe
 * write drops the rest of the buffer, and neither records what it saw. The
 * other two are answered by kill/fg/wait and by a MSG_CALL to yourself, which
 * no C program issues yet; named so the next one can tell them apart rather
 * than reading all three as "failed". Knowingly the C-only reading: ulib
 * folds this code into NO_FS for Rust programs, and the shell's "no such
 * task" is for a task index a user named, a different situation. */
#define TASK_ERR_NO_SUCH_TASK (~0UL - 14UL)
#define TASK_ERR_PROTECTED (~0UL - 15UL)
#define TASK_ERR_SELF (~0UL - 42UL)

/* Task ids + grant modes. */
#define FSD_TASK 2
#define CON_TASK 3
#define NET_TASK 4
#define GRANT_READ 1

/* Remote-mount relay (syscall-abi's NETOP_*): a NP message addressed to netd,
 * which carries it to the endpoint's 9P export over TCP and returns the NP
 * reply body verbatim. Offsets into the request: the 6-byte [ip:4][port:2 LE]
 * endpoint, then the NP message. */
#define NETOP_RMOUNT 4
#define NETOP_RMOUNT_ENDPOINT 8
#define NETOP_RMOUNT_MSG 16

#define HEAP_INFO_BASE 0
#define HEAP_INFO_SIZE 1
#define HEAP_INFO_STACK_BASE 2
#define HEAP_INFO_STACK_SIZE 3
#define HEAP_INFO_IMAGE_MAX 4

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
 * (mirrors syscall-abi's FS_ERR_MIN = u64::MAX - 43). Hand-mirrored, so it can
 * drift: the Rust constant moves DOWN whenever a new error code is reserved,
 * and a C caller compiled against a stale floor reads those new codes as
 * successful return values. syscall-abi's own definition carries a note back
 * to this line for that reason. */
/* Error codes a C program can distinguish. Only the ones it can act on -
 * everything else is just ">= FS_ERR_MIN". */
/* No filesystem answers there (nothing mounted, or a remote mount whose
 * peer netd could not reach: no route, refused, timed out), relayed as the
 * reply status, so a remote read on a downed peer names it instead of
 * reading as "failed". Mirrors syscall-abi's NO_FS. */
#define NO_FS (~0UL - 1UL)
#define FS_ERR_NOT_FOUND (~0UL - 2UL)
#define FS_ERR_PERM (~0UL - 32UL)
/* A request or reply that did not authenticate: no key for the peer, a reply
 * signature or keyed tag that did not verify. Named so a C caller can tell it
 * from a dead peer, which is the distinction the cluster auth design promises
 * (session-auth step 7: without it every keyed refusal printed "failed"). */
#define FS_ERR_AUTH (~0UL - 30UL)
#define FS_ERR_NO_SUCH_VERB (~0UL - 39UL)
/* A resource of the server is fully used for now (a session slot, or netd's
 * stack while it runs a cpu command, during which it refuses another local
 * client's requests rather than overflow): retry later. */
#define FS_ERR_BUSY (~0UL - 40UL)

#define FS_ERR_MIN (~0UL - 43UL)

/* A failure that never reached a server: no free fd slot, or a path this
 * library could not resolve. Deliberately NOT a wire value, so it sits BELOW
 * the band - nothing a server answers can be mistaken for it - and for the
 * same reason a reader must test for it BY NAME: ">= FS_ERR_MIN" is false for
 * it, which is how cremote's why() came to call every client-side refusal "no
 * error recorded". The floor moves down as codes are reserved; the assert
 * makes a move onto this value a build failure rather than an alias. */
#define FS_ERR_CLIENT (~0UL - 44UL)
_Static_assert(FS_ERR_CLIENT < FS_ERR_MIN,
               "FS_ERR_CLIENT must stay below the wire error band; the floor moved onto it");

/* The status of the last failed request (libc/src/file.c), and that status
 * as text, so every program prints the same words for the same code instead
 * of keeping a copy of the table. */
unsigned long ouro_last_fs_status(void);
const char *ouro_fs_strerror(void);

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
