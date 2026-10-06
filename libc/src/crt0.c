/* C runtime start: the loader's entry point. The kernel has already set up the
 * EL0 stack, so this builds the argument vector and the environment, runs main
 * and exits with its return code. Kept first in the image via .text.start
 * (programs/linker.ld).
 *
 * Both come from the kernel's per-task stores, the ones Rust programs read
 * through ulib, so a C program sees what the shell staged:
 *
 * - argv (GET_ARGC/GET_ARG): argv[0] the program's name as typed (`cargs`, not
 *   `/bin/CARGS`), argv[argc] NULL.
 * - environ (GET_ENVC/GET_ENV, since 2026-10-05, step 3 of the C-hosting
 *   plan): the `NAME=VALUE` strings `set` made, NULL-terminated, which is what
 *   picolibc's getenv reads. Until then a picolibc program calling getenv
 *   linked and was always told the variable was unset: picolibc's own libc.a
 *   defines `environ` (libc_stdlib_environ.c.o) as an empty vector, and
 *   nothing pointed it anywhere else.
 *
 * Built into static storage rather than the heap, so a program that never
 * mallocs keeps all of its heap, and so this file is the same for the
 * hand-rolled libc and the picolibc port, which both link it. A task spawned
 * with neither gets argc 0, argv = { NULL } and environ = { NULL }. */
#include <stdlib.h>
#include "sys.h"

extern int main(int argc, char **argv);

/* Both blobs are [count: u32] then [len: u32][bytes] per entry, so every entry
 * costs at least 4 bytes of the blob and argv's count can be at most
 * (ARGV_MAX - 4) / 4: an empty argument is a real one. An environment entry
 * is `NAME=VALUE`, at least two bytes, so 6 of the blob and a count of at
 * most (ENV_MAX - 4) / 6; a blob of shorter entries is not an environment,
 * and the vector stops at the cap. The strings here take len + 1 each (the
 * NUL for the length prefix), so they always fit in MAX bytes.
 *
 * The kernel stores a staged blob as given and the count call answers its
 * header, unchecked, so a spawner can claim any count. Both guards in
 * read_vec are live for such a blob: the count is clamped to what the blob
 * could hold, and the vector stops at the first entry the kernel cannot find,
 * so the vector counts the entries that exist. */
#define ARGC_CAP ((ARGV_MAX - 4) / 4)
#define ENVC_CAP ((ENV_MAX - 4) / 6)
static char *g_argv[ARGC_CAP + 1];
static char g_argbuf[ARGV_MAX];
static char *g_envp[ENVC_CAP + 1];
static char g_envbuf[ENV_MAX];

/* The environment getenv walks, set before main runs. Declared, not defined:
 * picolibc's libc.a defines it, and the hand-rolled libc in stdlib.c, weakly,
 * so a ported program that defines `char **environ;` itself still links. */
extern char **environ;

/* Reads the kernel's vector with `count_call`/`get_call` into `buf`, pointers
 * into `vec` (at most `cap` of them, then NULL), and returns how many.
 *
 * One copy per entry, into the rest of `buf`. The kernel caps the capacity
 * at its store's size rather than refusing a larger one, so NO_ARG means the
 * index is past the end (a refusal would need a buffer outside this task's
 * region, and these are its own statics). The strings of a blob the kernel
 * holds always fit in MAX bytes, so an entry never exceeds the room; should
 * one, it is left out rather than cut, and the entries after it are still
 * read: a `NAME=VALUE` cut short is a different value. No rig reaches that
 * branch (scripts/test-cenv.py says why). */
static int read_vec(long count_call, long get_call, char *buf, unsigned long size,
                    char **vec, unsigned long cap) {
    unsigned long n = (unsigned long)__os_syscall1(count_call, 0);
    if (n > cap) {
        n = cap; /* a header claiming more than the blob can hold */
    }
    unsigned long used = 0;
    int count = 0;
    for (unsigned long i = 0; i < n && used + 1 < size; i++) {
        char *dst = buf + used;
        unsigned long room = size - used - 1; /* room for the NUL */
        unsigned long len =
            (unsigned long)__os_syscall4(get_call, (long)i, (long)dst, (long)room, 0);
        if (len == NO_ARG) {
            break;
        }
        if (len > room) {
            continue;
        }
        dst[len] = '\0';
        vec[count++] = dst;
        used += len + 1;
    }
    vec[count] = NULL;
    return count;
}

__attribute__((section(".text.start"), used, noreturn)) void _start(void) {
    int argc =
        read_vec(SYS_GET_ARGC, SYS_GET_ARG, g_argbuf, sizeof g_argbuf, g_argv, ARGC_CAP);
    read_vec(SYS_GET_ENVC, SYS_GET_ENV, g_envbuf, sizeof g_envbuf, g_envp, ENVC_CAP);
    environ = g_envp;
    exit(main(argc, g_argv));
}
