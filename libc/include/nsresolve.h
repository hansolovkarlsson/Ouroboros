/* Namespace resolution for C, over the Rust `nsresolve` shim.
 *
 * The implementation is `ninep_abi::resolve_ns` - the SAME resolver ulib and
 * netd use, reached through a staticlib rather than copied into C. See
 * nsresolve/src/lib.rs for why (a copy would be behaviour, not constants, so
 * check-wire-constants' precedent does not carry) and what it cost (the link
 * needs --gc-sections; see the Makefile).
 */
#ifndef OURO_NSRESOLVE_H
#define OURO_NSRESOLVE_H

/* Low byte of `target`. For NS_TARGET_FSD the tree index is in bits 8..15. */
#define NS_TARGET_FSD 0
#define NS_TARGET_CONSOLE 1
#define NS_TARGET_NETLOCAL 2
#define NS_TARGET_REMOTE 3
#define NS_ENDPOINT_LEN 6

/* 0 on success, -1 if a buffer was too small. `endpoint` receives
 * [ip:4][port:2 LE] when the target is NS_TARGET_REMOTE. */
int ouro_ns_resolve(const char *path, unsigned long path_len,
                    char *out, unsigned long out_cap, unsigned long *out_len,
                    unsigned *target, unsigned char *endpoint);

#endif
