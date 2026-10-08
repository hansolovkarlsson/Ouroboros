/* The errno names the C probes print (cwinsz.c, ctermios.c), one list so
 * each probe does not grow its own copy that drifts. For the probes only, not
 * part of the C library. */
#ifndef OUROBOROS_PROBE_ERRNO_H
#define OUROBOROS_PROBE_ERRNO_H
#include <errno.h>

static inline const char *err_name(int e) {
    return e == ENOTTY ? "ENOTTY" : e == EBADF ? "EBADF" : e == EFAULT ? "EFAULT"
         : e == EINVAL ? "EINVAL" : e == EIO ? "EIO" : e == ENOSYS ? "ENOSYS" : "other";
}

#endif
