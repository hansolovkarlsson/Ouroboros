/* The first C program to run on Ouroboros — the foundational proof for the
 * userland-libc arc (docs/roadmap.md, "POSIX / C-program portability").
 *
 * Deliberately self-contained (its own _start + syscall stubs, no libc yet):
 * the point is to prove the *toolchain path* end to end — C cross-compiled to
 * our aarch64 PIE format, loaded by the existing loader, run through the same
 * syscall boundary the Rust programs use. Once this runs, growing toward a real
 * libc (picolibc/newlib) is adding stubs, not inventing the mechanism.
 *
 * Constraints inherited from the loader (programs/linker.ld):
 *   - entry `_start` at `.text.start` (offset 0);
 *   - NO .data / .bss (the linker script ASSERTs them empty — no support for
 *     initialized/zeroed statics yet), so: no globals, string literals only
 *     (they live in .rodata);
 *   - self-relocating PIE (-fPIC + -pie), R_AARCH64_RELATIVE processed at load.
 *
 * Syscall ABI (docs/architecture.md): number in x8, arg0 in x0, return in x0.
 */

#define SYS_PUTC 4
#define SYS_EXIT 17

/* One-argument system call: x8 = number, x0 = arg, returns x0. */
static inline long os_syscall1(long num, long arg0) {
    register long x8 asm("x8") = num;
    register long x0 asm("x0") = arg0;
    asm volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return x0;
}

static void os_putc(char c) {
    os_syscall1(SYS_PUTC, (unsigned char)c);
}

/* Write a NUL-terminated string to the console. (`write(2)` will layer on this
 * once there's a real libc; for now the loop is the whole "stdout".) */
static void os_puts(const char *s) {
    while (*s) {
        os_putc(*s++);
    }
}

__attribute__((noreturn)) static void os_exit(int code) {
    os_syscall1(SYS_EXIT, code);
    for (;;) {
    } /* EXIT never returns; keep the compiler's control-flow analysis happy */
}

/* Globals exercise the loader's .data/.bss support: `g_data` proves an
 * initialized global is loaded from the file (should read 7), `g_bss` proves an
 * uninitialized one is zeroed (should read 0), and mutating both proves the
 * region is writable. Before the loader supported these, the linker script's
 * ASSERTs rejected any non-empty .data/.bss. */
int g_data = 7;
int g_bss;

int main(void) {
    os_puts("hello from C on Ouroboros\r\n");

    os_puts("data=");
    os_putc('0' + g_data); /* 7, from .data */
    os_puts(" bss=");
    os_putc('0' + g_bss); /* 0, from .bss */
    os_puts("\r\n");

    g_data++;    /* mutate .data */
    g_bss = 5;   /* write .bss  */

    os_puts("after: data=");
    os_putc('0' + g_data); /* 8 */
    os_puts(" bss=");
    os_putc('0' + g_bss); /* 5 */
    os_puts("\r\n");
    return 0;
}

/* The entry point the loader jumps to (kept first via .text.start). The kernel
 * has already set up the EL0 stack, so this just runs main and exits. */
__attribute__((section(".text.start"), used, noreturn)) void _start(void) {
    int rc = main();
    os_exit(rc);
}
