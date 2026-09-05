/* Namespace resolution from C - the step-3 build gate, as a runnable program.
 *
 * Prints where each path resolves to, using the SAME resolver ulib and netd
 * use (`ninep_abi::resolve_ns`, reached through the `nsresolve` Rust shim).
 * That is the whole point: a C program could not previously answer this
 * question at all, which is why `open("/mnt/a/F")` went to fsd and never left
 * the machine.
 *
 * Built by `make nsdemo-bin`, staged as /bin/NSDEMO. Run it with no arguments
 * for a fixed set of paths, or pass paths to resolve.
 */
#include <stdio.h>
#include <string.h>

#define NS_TARGET_FSD      0
#define NS_TARGET_CONSOLE  1
#define NS_TARGET_NETLOCAL 2
#define NS_TARGET_REMOTE   3

int ouro_ns_resolve(const char *path, unsigned long path_len,
                    char *out, unsigned long out_cap, unsigned long *out_len,
                    unsigned *target, unsigned char *endpoint);

static void show(const char *p) {
    char out[256];
    unsigned long n = 0;
    unsigned t = 0;
    unsigned char ep[6];
    memset(ep, 0, sizeof ep);
    if (ouro_ns_resolve(p, strlen(p), out, sizeof out - 1, &n, &t, ep) != 0) {
        printf("%s -> resolve failed\r\n", p);
        return;
    }
    out[n] = 0;
    switch (t & 0xff) {
    case NS_TARGET_FSD:
        printf("%s -> fsd tree %d, path %s\r\n", p, (int)(t >> 8), out);
        break;
    case NS_TARGET_CONSOLE:
        printf("%s -> console\r\n", p);
        break;
    case NS_TARGET_NETLOCAL:
        printf("%s -> netd /net, path %s\r\n", p, out);
        break;
    case NS_TARGET_REMOTE:
        printf("%s -> REMOTE %d.%d.%d.%d:%d, path %s\r\n", p,
               ep[0], ep[1], ep[2], ep[3], ep[4] | (ep[5] << 8), out);
        break;
    default:
        printf("%s -> unknown target %u\r\n", p, t);
        break;
    }
}

/* `main(void)`, NOT `main(int, char **)`. crt0.c declares `extern int
 * main(void)` and calls it with no arguments - C programs on this system get no
 * argv at all yet (the Rust side has GET_ARGV; the C runtime does not wire it).
 * The first version of this file took argc/argv anyway, which is a signature
 * mismatch reading a garbage argc; it happened to take the no-arguments branch,
 * which is exactly how that class of bug survives a test. Fixed paths until the
 * C runtime grows argv. */
int main(void) {
    show("/EFI/ORBS/INIT.CFG");
    show("/mnt/a/HELLO.TXT");
    show("/dev/cons");
    show("/net/ip");
    return 0;
}
