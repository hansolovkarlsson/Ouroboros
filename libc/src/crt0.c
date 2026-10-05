/* C runtime start: the loader's entry point. The kernel has already set up the
 * EL0 stack, so this builds the argument vector, runs main and exits with its
 * return code. Kept first in the image via .text.start (programs/linker.ld).
 *
 * argv comes from the kernel's per-task store (GET_ARGC/GET_ARG), the same one
 * Rust programs read through ulib, so a C program sees the vector the shell
 * staged: argv[0] the program's name as typed (`cargs`, not `/bin/CARGS`),
 * argv[argc] NULL. Built into static
 * storage rather than the heap, so a program that never mallocs keeps all of
 * its heap, and so this file is the same for the hand-rolled libc and the
 * picolibc port, which both link it. A task spawned with no argv (or a staged
 * blob the kernel does not hold) gets argc 0 and argv = { NULL }. */
#include <stdlib.h>
#include "sys.h"

extern int main(int argc, char **argv);

/* The kernel's blob is [argc: u32] then [len: u32][bytes] per argument, at most
 * ARGV_MAX bytes, so every argument costs at least 4 bytes of it and argc can
 * be at most (ARGV_MAX - 4) / 4. The strings here take len + 1 each (the NUL
 * for the length prefix), so they always fit in ARGV_MAX.
 *
 * The kernel stores a staged blob as given and GET_ARGC answers its header,
 * unchecked, so a spawner can claim any count. Both guards below are live for
 * such a blob: argc is clamped to what the blob could hold, and the vector
 * stops at the first argument GET_ARG cannot find, so main's argc counts the
 * arguments that exist (a Rust program reading GET_ARGC sees the claim). */
#define ARGC_CAP ((ARGV_MAX - 4) / 4)
static char *g_argv[ARGC_CAP + 1];
static char g_argbuf[ARGV_MAX];

static int build_argv(void) {
    unsigned long n = (unsigned long)__os_syscall1(SYS_GET_ARGC, 0);
    if (n > ARGC_CAP) {
        n = ARGC_CAP; /* a header claiming more than the blob can hold */
    }
    unsigned long used = 0;
    int argc = 0;
    for (unsigned long i = 0; i < n && used < sizeof g_argbuf; i++) {
        char *dst = g_argbuf + used;
        unsigned long cap = sizeof g_argbuf - used - 1; /* room for the NUL */
        unsigned long len =
            (unsigned long)__os_syscall4(SYS_GET_ARG, (long)i, (long)dst, (long)cap, 0);
        if (len == NO_ARG) {
            break;
        }
        if (len > cap) {
            len = cap; /* the kernel copied only cap bytes */
        }
        dst[len] = '\0';
        g_argv[argc++] = dst;
        used += len + 1;
    }
    g_argv[argc] = NULL;
    return argc;
}

__attribute__((section(".text.start"), used, noreturn)) void _start(void) {
    int argc = build_argv();
    exit(main(argc, g_argv));
}
