/* C file I/O over the filesystem server (fsd) using fids - server-side
 * open-file handles (a POSIX fd IS a 9P fid). open() establishes a fid in fsd
 * (which authorizes the access once, against the file's mode/owner), and the fd
 * a C program holds *is* that fid; read/write/stat/close reference it. The
 * cursor stays client-side and rides each read/write offset (authentic 9P).
 *
 * Also here: stdout-target routing, so write(1|2) goes to the console or, when
 * the program is a pipe producer, to the consumer - so a C program works in a
 * pipeline.
 *
 * PATHS RESOLVE THROUGH THIS TASK'S NAMESPACE, not just against the cwd. Until
 * 2026-09-05 every request went to fsd unconditionally, so open("/mnt/a/F")
 * asked fsd about a path only netd knows - a remote mount is a NAMESPACE
 * binding, not an fsd mount - and the request never left the machine. That was
 * the actual cause of "the fid verbs reach no export"; see
 * docs/roadmap/roadmap-fid-verbs.md.
 *
 * AND THE FD IS NO LONGER THE FID. The old design ("a POSIX fd IS a 9P fid")
 * was exact while fsd was the only server that could issue one. It cannot
 * survive a second: a remote fid 3 from the far export and a local fid 3 from
 * fsd are different handles wearing the same number, and both indexed the same
 * g_files slot. The fd is now a C-chosen slot and the server's fid is stored
 * beside the target it belongs to - the same reason netd must own remote fids
 * rather than pass fsd's through (decision 2 in that document), arriving from
 * the client side. */
#include "sys.h"
#include "nsresolve.h"
#include <fcntl.h>
#include <string.h>
#include <stdarg.h>
/* By its path, not <sys/ioctl.h>: the one copy lives with the picolibc-side
 * headers (libc/pico/include), which the hand-rolled build of this file does
 * not search, and libc/include must stay off a picolibc build's angle path. */
#include "../pico/include/sys/ioctl.h"
#include <sys/stat.h>
#include <unistd.h>
/* `errno` where the C library has one: picolibc does, and the Makefile builds
 * this file for a picolibc program (`$(BUILD_DIR)/pico/file.o`) with
 * -DOURO_HAVE_ERRNO, said by the build rather than guessed from the include
 * path (the review of #215). The hand-rolled libc has none, so there every
 * call reports through `ouro_last_fs_status` alone. */
#ifdef OURO_HAVE_ERRNO
#include <errno.h>
#endif
#include <limits.h>

/* How many files this LIBRARY can hold open. No longer tied to fsd's MAX_FIDS:
 * that number justified the old fd==fid identity, which is retired (see the
 * header note), and a slot here is consumed by a local OR a remote fid, so the
 * two counts are about different things. Do not "resync" them. */
#define MAX_FILES 8
/* Room for a path AFTER namespace substitution, which can be LONGER than what
 * the caller passed: a binding replaces its prefix with a target root that may
 * be longer (`bind /m /very/long/root`). ulib uses 256 (FSP_MAX) for the same
 * buffer; 96 silently truncated, and a truncated path with O_TRUNC truncates
 * the WRONG FILE with no error anywhere. */
#define PATH_MAX_C 96
#define FSPATH_MAX_C 256

/* Per-open client state. Indexed by (fd - FID_BASE), a slot THIS library
 * chooses - see the note above on why the fd is no longer the fid. */
struct file_state {
    int used;
    long offset;
    int flags;
    unsigned long fid;      /* the fid the SERVER issued, which may collide
                             * across servers - hence a separate slot number */
    unsigned target;        /* NS_TARGET_* of the mount this fid lives on */
    unsigned char endpoint[NS_ENDPOINT_LEN]; /* NS_TARGET_REMOTE only */
};
static struct file_state g_files[MAX_FILES];

/* The status of the last failed request. Written when the hand-rolled libc
 * was the only one and had no errno, so without this a C program could not
 * tell "no such file" from "that server does not implement this request" -
 * the exact confusion FS_ERR_NO_SUCH_VERB was reserved to end, stopping one
 * layer short of C. Still the only report there; a picolibc program has errno
 * as well (set_errno_from_status), and this keeps the server's own code where
 * errno can only say EIO. Not errno: one value, no thread story.
 *
 * Cleared to 0 by every request a server answers without error (np_request's
 * success arm), or a later failure that sets nothing reports whatever the
 * previous call left behind - a stale answer being worse than none here. The
 * comment said "cleared on success" for two days before anything did it.
 *
 * Client-side failures (no fd slot, an unresolvable path) record FS_ERR_CLIENT,
 * defined in sys.h next to the wire codes it must stay distinct from. */
static unsigned long g_last_status;

unsigned long ouro_last_fs_status(void) {
    return g_last_status;
}

/* A status as words. Only the codes a C program can act on are named; the
 * rest of the band is "failed", and 0 is the explicit "nothing recorded" a
 * caller must not mistake for an error. Was a private why() copied into each
 * program, and the copies had already drifted on one wording. */
const char *ouro_fs_strerror(void) {
    unsigned long s = g_last_status;
    if (s == NO_FS) return "no filesystem there, or the peer cannot be reached";
    if (s == FS_ERR_NOT_FOUND) return "no such file or directory";
    if (s == FS_ERR_PERM) return "permission denied";
    if (s == FS_ERR_AUTH) return "authentication failed (no key for that peer, or its reply did not verify)";
    if (s == FS_ERR_NO_SUCH_VERB) return "that server does not implement this request, or not on this kind of connection";
    if (s == MSG_ERR_DENIED) return "not allowed to reach that server (capability)";
    if (s == FS_ERR_BUSY) return "that server is out of room for this right now (try again later)";
    if (s == TASK_ERR_NO_SUCH_TASK) return "that server is not running (it died with this request in flight, or was absent this boot)";
    if (s == FS_ERR_CLIENT) return "never sent (refused by the library: a bad fd or argument, no free fd, or a path too long)";
    if (s >= FS_ERR_MIN) return "failed";
    return "no error recorded";
}

/* ---- fsd request helper -------------------------------------------------- */

/* Send a ninep request to the server a resolved path lives on, and copy up to
 * `reply_cap` bytes of the reply DATA (after the 8-byte status) into
 * `reply_data`. Returns the server's status.
 *
 * `target`/`endpoint` say where: NS_TARGET_FSD goes straight to fsd as before;
 * NS_TARGET_REMOTE is wrapped in a NETOP_RMOUNT to netd, which carries the NP
 * message to that endpoint's export over TCP and returns the reply body
 * verbatim - so netd's relay needs no knowledge of the fid verbs (it is
 * verb-agnostic; the far side is what must implement them).
 *
 * The wrapping is byte-for-byte ulib::np_remote's, because it is the same wire:
 * two implementations of one frame is exactly the drift check-wire-constants
 * exists for, and it only pins the constants, not the layout. */
static long np_request(unsigned target, const unsigned char *endpoint,
                       unsigned long verb, unsigned long p0, unsigned long p1, unsigned long p2,
                       const char *path, size_t pathlen, unsigned char *reply_data,
                       size_t reply_cap) {
    unsigned char req[MSG_MAX_LEN];
    unsigned base = 0;
    long dest = FSD_TASK;
    memset(req, 0, NETOP_RMOUNT_MSG + NP_REQ_PAYLOAD);
    if (pathlen > FS_DATA_MAX) {
        pathlen = FS_DATA_MAX;
    }
    /* Only the header needs clearing - the payload region is written by the
     * memcpy below or not sent at all. Zeroing all 768 bytes per call cost
     * ~150K pointless byte stores on a 100 KB read (200 chunks x 768). */
    if ((target & 0xff) == NS_TARGET_REMOTE) {
        dest = NET_TASK;
        base = NETOP_RMOUNT_MSG;
        __wr_u64(req + 0, NETOP_RMOUNT);
        memcpy(req + NETOP_RMOUNT_ENDPOINT, endpoint, NS_ENDPOINT_LEN);
    }
    __wr_u64(req + base + 0, verb);
    /* The fsd TREE INDEX, from `target`'s high bits - a resolved path may live
     * on a mounted partition, not just the boot disk, and C could not address
     * one before.
     *
     * ulib::np_remote hardcodes 0 here with a stated reason (a remote export
     * serves its own boot mount). This agrees with it in practice because
     * `nsresolve` sets no high bits for NsTarget::Remote, so the remote case
     * writes 0 either way - but it agrees by derivation rather than by
     * assertion, which is the safer of the two. */
    __wr_u64(req + base + 8, (unsigned long)((target >> 8) & 0xff));
    __wr_u64(req + base + 16, p0);
    __wr_u64(req + base + 24, p1);
    __wr_u64(req + base + 32, p2);
    if (pathlen && path) {
        memcpy(req + base + NP_REQ_PAYLOAD, path, pathlen);
    }
    unsigned char reply[MSG_MAX_LEN];
    /* The delegation race, ridden out as ulib::net_msg_call does. The shell
     * SPAWNs a program and only then DELEGATEs it TO_NET (it cannot delegate
     * to a slot that does not exist yet), so a C program whose first remote
     * op reaches netd inside that window is refused MSG_ERR_DENIED. That is
     * transient by construction: the delegation is already on its way. This
     * call returned it as a failure instead, and the first cbig after a mount
     * failed "not allowed to reach that server (capability)" about one boot
     * in three on the two-node rig (2026-09-23). Only for netd, as in ulib:
     * a denial from fsd is not this race. The same 150-tick bound. */
    long deadline = __os_syscall1(SYS_GET_TICKS, 0) + 150;
    long r;
    for (;;) {
        r = __os_syscall4(SYS_MSG_CALL, dest, (long)req,
                          (long)(base + NP_REQ_PAYLOAD + pathlen), (long)reply);
        if (dest == NET_TASK && (unsigned long)r == MSG_ERR_DENIED &&
            __os_syscall1(SYS_GET_TICKS, 0) <= deadline) {
            continue;
        }
        break;
    }
    if ((unsigned long)r >= FS_ERR_MIN) {
        /* A transport failure, not a server answer: a denial that outlived
         * the delegation window, or any other refusal of the call itself. */
        g_last_status = (unsigned long)r;
        return (long)FS_ERR_MIN;
    }
    unsigned long rlen = (unsigned long)r & 0xffffffffUL;
    if (reply_data && rlen > 8) {
        size_t d = rlen - 8;
        if (d > reply_cap) {
            d = reply_cap;
        }
        memcpy(reply_data, reply + 8, d);
    }
    long status = (long)__rd_u64(reply);
    g_last_status = (unsigned long)status >= FS_ERR_MIN ? (unsigned long)status : 0;
    return status;
}

/* ---- path resolution (for open) ------------------------------------------ */

/* The absolute form of `path` into `out`, or -1 when it does not fit.
 *
 * It used to TRUNCATE at PATH_MAX_C - 1 and carry on, so a long path opened
 * whatever its first 95 bytes named. Measured 2026-09-07 with a file whose
 * path is exactly 95 bytes and a 98-byte path built on it: O_RDONLY read that
 * file, O_WRONLY|O_TRUNC emptied it, exit 0, no message. A cwd the kernel
 * reports as too long fails the same way rather than being replaced by "/";
 * only a cwd the kernel does not know at all falls back to the root. */
static int resolve_path_raw(const char *path, char *out);

/* Collapses `out` in place: empty and `.` components dropped, `..` removing
 * the one before it (never above the root), so `/mnt/f/../x` is `/x`, the
 * path it means, before the namespace picks its mount. `ulib::resolve` does
 * the same for Rust programs; without it, a `..` could choose the wrong tree,
 * and a rename's same-mount check compare the wrong ones (the review of
 * #215). */
static void normalize_path(char *out) {
    size_t r = 0;
    size_t w = 0;
    size_t len = strlen(out);
    while (r < len) {
        while (r < len && out[r] == '/') {
            r++;
        }
        size_t start = r;
        while (r < len && out[r] != '/') {
            r++;
        }
        size_t n = r - start;
        if (n == 0 || (n == 1 && out[start] == '.')) {
            continue;
        }
        if (n == 2 && out[start] == '.' && out[start + 1] == '.') {
            while (w > 0 && out[w - 1] != '/') {
                w--;
            }
            if (w > 0) {
                w--;
            }
            continue;
        }
        out[w++] = '/';
        memmove(out + w, out + start, n);
        w += n;
    }
    if (w == 0) {
        out[w++] = '/';
    }
    out[w] = 0;
}

static int resolve_path(const char *path, char *out) {
    int r = resolve_path_raw(path, out);
    if (r == 0) {
        normalize_path(out);
    }
    return r;
}

static int resolve_path_raw(const char *path, char *out) {
    if (path[0] == '/') {
        size_t n = strlen(path);
        if (n >= PATH_MAX_C) {
            return -1;
        }
        memcpy(out, path, n);
        out[n] = 0;
        return 0;
    }
    char cwd[PATH_MAX_C];
    long clen = __os_syscall4(SYS_GET_CWD, (long)cwd, sizeof(cwd), 0, 0);
    if (clen <= 0) {
        clen = 1;
        cwd[0] = '/';
    }
    if (clen >= PATH_MAX_C) {
        return -1;
    }
    size_t o = (size_t)clen;
    memcpy(out, cwd, o);
    if (out[o - 1] != '/') {
        out[o++] = '/';
    }
    size_t n = strlen(path);
    if (o + n >= PATH_MAX_C) {
        return -1;
    }
    memcpy(out + o, path, n);
    out[o + n] = 0;
    return 0;
}

/* Where `path` lives: the server-side path, its length, the target (server,
 * and the fsd tree in its high bits) and, for a remote mount, the endpoint.
 * 0, or -1 with FS_ERR_CLIENT recorded (too long, or not resolvable), or
 * FS_ERR_NOT_FOUND for an empty path. Every target a binding can name, the
 * console and `/net` included: `stat` answers the console itself, and
 * resolve_target below refuses both for the verbs that need a file. */
static int resolve_any(const char *path, char *fspath, unsigned long *fslen,
                       unsigned *target, unsigned char *endpoint) {
    char abspath[PATH_MAX_C];
    /* An empty path names nothing (POSIX: ENOENT). It used to resolve to the
     * cwd, harmless for open, not for unlink or a rename of "" (the review of
     * #215). */
    if (path[0] == 0) {
        g_last_status = FS_ERR_NOT_FOUND;
        return -1;
    }
    if (resolve_path(path, abspath) != 0) {
        g_last_status = FS_ERR_CLIENT;
        return -1;
    }
    memset(endpoint, 0, NS_ENDPOINT_LEN);
    *fslen = 0;
    *target = 0;
    if (ouro_ns_resolve(abspath, strlen(abspath), fspath, FSPATH_MAX_C, fslen,
                        target, endpoint) != 0) {
        g_last_status = FS_ERR_CLIENT;
        return -1;
    }
    return 0;
}

/* resolve_any, refusing the targets with no file model, the console and
 * `/net`, where "no such file" would be a lie (FS_ERR_NO_SUCH_VERB). For the
 * verbs that need a file: open, unlink, rename and the rest. */
static int resolve_target(const char *path, char *fspath, unsigned long *fslen,
                          unsigned *target, unsigned char *endpoint) {
    if (resolve_any(path, fspath, fslen, target, endpoint) != 0) {
        return -1;
    }
    if ((*target & 0xff) == NS_TARGET_CONSOLE || (*target & 0xff) == NS_TARGET_NETLOCAL) {
        g_last_status = FS_ERR_NO_SUCH_VERB;
        return -1;
    }
    return 0;
}

/* Sets `errno` from the status the last request recorded, where there is an
 * `errno` (see the include above). Every call in this file that returns -1
 * sets it, through this or through client_fail below (step 2 of the C-hosting
 * plan, 2026-10-05; until then only the path verbs did). Proem's include
 * search depends on it: ENOENT or ENOTDIR means "try the next directory", and
 * anything else stops the search as an error. */
static void set_errno_from_status(void) {
#ifdef OURO_HAVE_ERRNO
    unsigned long s = g_last_status;
    if (s == FS_ERR_NOT_FOUND) errno = ENOENT;
    else if (s == FS_ERR_PERM || s == FS_ERR_AUTH || s == MSG_ERR_DENIED) errno = EACCES;
    else if (s == FS_ERR_NOT_A_FILE) errno = EISDIR;
    else if (s == FS_ERR_NOT_A_DIRECTORY) errno = ENOTDIR;
    else if (s == FS_ERR_INVALID_NAME) errno = EINVAL;
    else if (s == FS_ERR_ALREADY_EXISTS) errno = EEXIST;
    else if (s == FS_ERR_NOT_EMPTY) errno = ENOTEMPTY;
    else if (s == FS_ERR_IS_ROOT || s == FS_ERR_BUSY) errno = EBUSY;
    else if (s == FS_ERR_DISK_FULL) errno = ENOSPC;
    else if (s == FS_ERR_READ_ONLY) errno = EROFS;
    else if (s == FS_ERR_CROSS_DEVICE) errno = EXDEV;
    else if (s == FS_ERR_NO_SUCH_VERB) errno = ENOSYS;
    else if (s == NO_FS || s == TASK_ERR_NO_SUCH_TASK) errno = ENODEV;
    else if (s == FS_ERR_CLIENT) errno = ENAMETOOLONG;
    else errno = EIO;
#endif
}

/* A failure the library finds without asking a server (a bad fd, a full fd
 * table, a bad argument): POSIX's own errno for it, and FS_ERR_CLIENT for
 * ouro_last_fs_status, which says "never sent" and must not keep the status
 * of whatever request came before. Always -1, for the caller to return. */
static int client_fail_(int e) {
    g_last_status = FS_ERR_CLIENT;
#ifdef OURO_HAVE_ERRNO
    errno = e;
#else
    (void)e;
#endif
    return -1;
}
/* A macro so the hand-rolled build, which has no <errno.h>, never expands the
 * errno name at all, rather than this file defining stand-ins for names a
 * header could later define for real (the review of #220). */
#ifdef OURO_HAVE_ERRNO
#define client_fail(e) client_fail_(e)
#else
#define client_fail(e) client_fail_(0)
#endif

/* errno for a failed read or write on an fd opened without that mode. fsd
 * refuses such a request (the fid's flags are its authority, and a raw 9P
 * peer has no library in front), and POSIX names that refusal EBADF, not the
 * EACCES FS_ERR_PERM maps to. So the request is still sent, and only that
 * answer is renamed: any other status (the disk unmounted, the server gone,
 * the peer unreachable) is mapped as it is, since it says something else
 * (the review of #220). */
static void set_errno_mode(int opened_for_it) {
    set_errno_from_status();
#ifdef OURO_HAVE_ERRNO
    if (!opened_for_it && g_last_status == FS_ERR_PERM) {
        errno = EBADF;
    }
#else
    (void)opened_for_it;
#endif
}

/* ---- open / close / lseek / stat / fstat / unlink / rename -------------- */

int open(const char *path, int flags, ...) {

    unsigned long oflags = 0;
    int acc = flags & 3; /* O_ACCMODE: O_RDONLY=0, O_WRONLY=1, O_RDWR=2 */
    if (acc == O_WRONLY || acc == O_RDWR) {
        oflags |= OPEN_WRITE;
    }
    if (acc == O_RDONLY || acc == O_RDWR) {
        oflags |= OPEN_READ;
    }
    if (flags & O_CREAT) {
        oflags |= OPEN_CREATE;
    }
    if (flags & O_TRUNC) {
        oflags |= OPEN_TRUNC;
    }

    /* Where does this path live? Until this call existed, the answer was
     * always "fsd", which is why a remote mount was unreachable from C. */
    char fspath[FSPATH_MAX_C];
    unsigned long fslen;
    unsigned target;
    unsigned char endpoint[NS_ENDPOINT_LEN];
    if (resolve_target(path, fspath, &fslen, &target, endpoint) != 0) {
        set_errno_from_status();
        return -1;
    }

    /* A free slot of OUR choosing - see the header note: the fd cannot be the
     * fid once two servers can issue them. */
    int idx = -1;
    for (int i = 0; i < MAX_FILES; i++) {
        if (!g_files[i].used) {
            idx = i;
            break;
        }
    }
    if (idx < 0) {
        return client_fail(EMFILE);
    }

    /* NP_OPEN's a0 is the FLAGS and a1 the path length - the reverse of every
     * other path-carrying verb. Getting this backwards resolves a 1-3 byte path
     * out of the flag word and lands somewhere plausible instead of failing. */
    long fid = np_request(target, endpoint, NP_OPEN, oflags, fslen, 0,
                          fspath, (size_t)fslen, 0, 0);
    if ((unsigned long)fid >= FS_ERR_MIN || fid < FID_BASE) {
        set_errno_from_status(); /* EIO for a fid below FID_BASE: no status */
        return -1;
    }
    g_files[idx].used = 1;
    g_files[idx].flags = flags;
    g_files[idx].offset = 0;
    g_files[idx].fid = (unsigned long)fid;
    g_files[idx].target = target;
    memcpy(g_files[idx].endpoint, endpoint, NS_ENDPOINT_LEN);
    return FID_BASE + idx;
}

/* Removes the file at `path` (`NP_RM`, as `rm` sends it), so picolibc's
 * `remove` links and works: Proem removes its `-o` output after a failed run.
 * 0, or -1 with `errno` set where there is one (ENOENT for a missing file). A
 * directory is refused (`rmdir` is its verb). Since 2026-10-05. */
int unlink(const char *path) {
    char fspath[FSPATH_MAX_C];
    unsigned long fslen;
    unsigned target;
    unsigned char endpoint[NS_ENDPOINT_LEN];
    if (resolve_target(path, fspath, &fslen, &target, endpoint) != 0) {
        set_errno_from_status();
        return -1;
    }
    long s = np_request(target, endpoint, NP_RM, fslen, 0, 0, fspath, (size_t)fslen, 0, 0);
    if ((unsigned long)s >= FS_ERR_MIN) {
        set_errno_from_status();
        return -1;
    }
    return 0;
}

/* Renames `oldpath` to `newpath` (`NP_MV`, the two paths back to back), and
 * when `newpath` is an existing ordinary file, replaces it: on ext2 that is one
 * directory-entry write, on FAT32 and exFAT two, ordered so that the name
 * always finds one of the two files (`mv`'s replace, 2026-09-02). Edit saves
 * through it, writing a temp file and renaming it over the original. Both
 * paths must live on the same server and tree, or -1 with EXDEV, as POSIX
 * answers a rename across mounts. Since 2026-10-05. */
int rename(const char *oldpath, const char *newpath) {
    char src[FSPATH_MAX_C];
    char dst[FSPATH_MAX_C];
    unsigned long srclen;
    unsigned long dstlen;
    unsigned srctarget;
    unsigned dsttarget;
    unsigned char srcend[NS_ENDPOINT_LEN];
    unsigned char dstend[NS_ENDPOINT_LEN];
    if (resolve_target(oldpath, src, &srclen, &srctarget, srcend) != 0 ||
        resolve_target(newpath, dst, &dstlen, &dsttarget, dstend) != 0) {
        set_errno_from_status();
        return -1;
    }
    if (srctarget != dsttarget || memcmp(srcend, dstend, NS_ENDPOINT_LEN) != 0) {
        /* The code ulib::fs_mv and the shell record for the same refusal, so
         * a program without errno can tell it from a too-long path. */
        g_last_status = FS_ERR_CROSS_DEVICE;
        set_errno_from_status();
        return -1;
    }
    char both[2 * FSPATH_MAX_C];
    if (srclen + dstlen > FS_DATA_MAX) {
        g_last_status = FS_ERR_CLIENT;
        set_errno_from_status();
        return -1;
    }
    memcpy(both, src, srclen);
    memcpy(both + srclen, dst, dstlen);
    long s = np_request(srctarget, srcend, NP_MV, srclen, dstlen, 0, both,
                        (size_t)(srclen + dstlen), 0, 0);
    if ((unsigned long)s >= FS_ERR_MIN) {
        set_errno_from_status();
        return -1;
    }
    return 0;
}

/* Removes the empty directory at `path` (`NP_RMDIR`, as `rmdir` sends it). 0,
 * or -1 with `errno` (ENOTEMPTY, ENOTDIR, ENOENT). Since 2026-10-05. */
int rmdir(const char *path) {
    char fspath[FSPATH_MAX_C];
    unsigned long fslen;
    unsigned target;
    unsigned char endpoint[NS_ENDPOINT_LEN];
    if (resolve_target(path, fspath, &fslen, &target, endpoint) != 0) {
        set_errno_from_status();
        return -1;
    }
    long s = np_request(target, endpoint, NP_RMDIR, fslen, 0, 0, fspath, (size_t)fslen, 0, 0);
    if ((unsigned long)s >= FS_ERR_MIN) {
        set_errno_from_status();
        return -1;
    }
    return 0;
}

/* POSIX `remove`: a file through `unlink`, an empty directory through `rmdir`.
 * picolibc's own calls `unlink` alone, so `remove` of a directory answered
 * EISDIR; this one, linked ahead of picolibc's archive, replaces it (the
 * review of #215). */
int remove(const char *path) {
    if (unlink(path) == 0) {
        return 0;
    }
    if (g_last_status != FS_ERR_NOT_A_FILE) {
        return -1;
    }
    return rmdir(path);
}

static struct file_state *file_for(int fd) {
    int idx = fd - FID_BASE;
    if (idx < 0 || idx >= MAX_FILES || !g_files[idx].used) {
        return 0;
    }
    return &g_files[idx];
}

/* Close every open fd at exit.
 *
 * A fid is server-side state in fsd, and nothing else releases it promptly. fsd
 * reaps a leaked fid only when its table is full, by noticing that the owning
 * task's slot now holds a different occupant (a generation, since 2026-09-12;
 * before that it asked whether the SLOT was dead, and the shell reuses the same
 * one for every foreground command, so eight C programs that each leaked a fid
 * left the ninth unable to open anything until fsd restarted). Closing at exit
 * is still right: a fid held until the table fills is a slot some other
 * program cannot have in the meantime. libc/cleak.c is the program that
 * deliberately skips this, the check for fsd's reaper. */
void __libc_close_all(void) {
    for (int i = 0; i < MAX_FILES; i++) {
        if (g_files[i].used) {
            close(i + FID_BASE);
        }
    }
}

/* The slot is released whatever the server answers, as POSIX close releases
 * the descriptor even when it reports an error; a refused NP_CLUNK (a peer
 * gone, a server restarted) is then -1 with errno, where it used to be lost
 * (the review of #220). The console fds have no server state: closing one
 * succeeds and changes nothing, since write(1) and write(2) route by the
 * task's stdout target, not by a slot. */
int close(int fd) {
    if (fd >= 0 && fd < FID_BASE) {
        return 0;
    }
    struct file_state *f = file_for(fd);
    if (!f) {
        return client_fail(EBADF);
    }
    long s = np_request(f->target, f->endpoint, NP_CLUNK, f->fid, 0, 0, 0, 0, 0, 0);
    f->used = 0;
    if ((unsigned long)s >= FS_ERR_MIN) {
        set_errno_from_status();
        return -1;
    }
    return 0;
}

/* `st` from an `NP_STAT`/`NP_FSTAT` record: what the server's record says, and
 * nothing else, the one decoding `stat` and `fstat` share. The whole
 * `struct stat` is zeroed first: it used to be left as the caller's stack had
 * it but for two fields, and Proem, which knows a file by `st_dev` and
 * `st_ino`, could take two headers for one and skip the second silently (its
 * fstat-identity handoff, 2026-10-01). So `st_dev` and `st_ino` read 0, "no
 * identity", until the record carries one (a wire change, on the roadmap):
 * for `stat` as for `fstat`, two different files compare EQUAL by the usual
 * same-file test, (st_dev, st_ino), and a ported program must not use it.
 * Mode, uid and gid are the disk's where it records them (ext2,
 * `STAT_MODEVALID_OFF`). Where it does not (FAT32, exFAT, /proc), `st_mode`
 * is the file type from the record's directory flag with 0666 for a file and
 * 0777 for a directory: such a filesystem keeps no owner or mode, and fsd
 * lets every access through on it, so "anyone may read and write" is the
 * true answer, where 0000 would have told a ported program it may touch
 * nothing (the review of #216; Linux's vfat makes the bits up the same way,
 * from its mount options). uid and gid stay 0 there. Before, `st_mode` was 0
 * on those, so `S_ISREG` was false for every file.
 *
 * `info` must be zeroed before the request: the reply copies only what it
 * carries, and a shorter record (an older peer's 20 bytes) must read as "no
 * mode", not as the stack's leftovers in the mode-valid byte (the review of
 * #216). */
static void stat_from_record(struct stat *st, const unsigned char *info) {
    memset(st, 0, sizeof *st);
    st->st_size = (long)__rd_u64(info + STAT_SIZE_OFF);
    if (info[STAT_MODEVALID_OFF]) {
        st->st_mode = (unsigned)(info[STAT_MODE_OFF] | (info[STAT_MODE_OFF + 1] << 8));
        st->st_uid = (unsigned)(info[STAT_UID_OFF] | (info[STAT_UID_OFF + 1] << 8));
        st->st_gid = (unsigned)(info[STAT_GID_OFF] | (info[STAT_GID_OFF + 1] << 8));
    } else {
        /* The whole u32, as ulib's stat_is_dir reads it. */
        unsigned long flags = (unsigned long)info[STAT_FLAGS_OFF] |
                              ((unsigned long)info[STAT_FLAGS_OFF + 1] << 8) |
                              ((unsigned long)info[STAT_FLAGS_OFF + 2] << 16) |
                              ((unsigned long)info[STAT_FLAGS_OFF + 3] << 24);
        st->st_mode = (flags & STAT_FLAG_DIR) ? (S_IFDIR | 0777) : (S_IFREG | 0666);
    }
}

/* The metadata of the file at `path`, without opening it: one NP_STAT, the
 * path verb `ls -l` uses, served by fsd, netd's export and the host peer
 * alike. 0, or -1 with errno set where there is one. Step 4 of
 * docs/roadmap/roadmap-c-hosting.md, since 2026-10-06; picolibc references it.
 *
 * Not open + fstat + close, which the plan first said. That would ask for
 * READ permission on the file and an fd slot, where POSIX's stat needs
 * neither: fsd authorizes NP_STAT by the ancestor walk alone (every directory
 * above must be searchable) and does not check the file's own mode, so `stat`
 * of a file the caller may not read succeeds, as it must, and works with every
 * fd in use. A directory needs no open either.
 *
 * A console binding (`mount -c`) is the character device fstat(1) reports,
 * at the binding itself only: a path below it names nothing (ENOENT). A
 * `/net` binding (`mount -n`) answers ENOSYS, as for open: netd serves
 * NP_STAT for `/`, `ip` and `mac` but not under `/tcp`, where the request
 * reaches the dial files' handler and gets EISDIR, EIO or ENOENT for files
 * that exist (and bumps a connection's idle clock). Until netd answers there
 * (on the roadmap), no answer beats a wrong one (the reviews of #223).
 *
 * A path ending in `/` or `/.` names a directory, so anything else answers
 * ENOTDIR (POSIX). The library collapses `.` and `..` as text before the
 * server sees the path (normalize_path), which drops that ending, so it is
 * checked here against the path as given. `..` stays lexical, as in Plan 9
 * and as for every call here: `/NOSUCH/../x` is `/x` (on the roadmap). */
int stat(const char *path, struct stat *st) {
    if (!path || !st) {
        return client_fail(EFAULT);
    }
    char fspath[FSPATH_MAX_C];
    unsigned long fslen;
    unsigned target;
    unsigned char endpoint[NS_ENDPOINT_LEN];
    if (resolve_any(path, fspath, &fslen, &target, endpoint) != 0) {
        set_errno_from_status();
        return -1;
    }
    size_t plen = strlen(path);
    int want_dir = path[plen - 1] == '/' ||
                   (path[plen - 1] == '.' && (plen == 1 || path[plen - 2] == '/'));
    if ((target & 0xff) == NS_TARGET_CONSOLE) {
        /* The binding resolves to "/"; anything longer is below it. */
        if (fslen != 1 || fspath[0] != '/') {
            g_last_status = FS_ERR_NOT_FOUND;
            set_errno_from_status();
            return -1;
        }
        if (want_dir) {
            g_last_status = FS_ERR_NOT_A_DIRECTORY;
            set_errno_from_status();
            return -1;
        }
        memset(st, 0, sizeof *st);
        st->st_mode = S_IFCHR | 0666; /* as fstat of a console fd */
        g_last_status = 0;
        return 0;
    }
    if ((target & 0xff) == NS_TARGET_NETLOCAL) {
        g_last_status = FS_ERR_NO_SUCH_VERB;
        set_errno_from_status();
        return -1;
    }
    unsigned char info[STAT_INFO_LEN] = {0};
    long s = np_request(target, endpoint, NP_STAT, fslen, 0, 0, fspath, (size_t)fslen, info,
                        sizeof(info));
    if ((unsigned long)s >= FS_ERR_MIN) {
        set_errno_from_status();
        return -1;
    }
    stat_from_record(st, info);
    if (want_dir && !S_ISDIR(st->st_mode)) {
        g_last_status = FS_ERR_NOT_A_DIRECTORY;
        set_errno_from_status();
        return -1;
    }
    return 0;
}

/* stat: no Ouroboros filesystem has symbolic links, so there is no link for
 * lstat to stop at. picolibc declares it, and a ported program that calls it
 * would otherwise fail to link (the review of #223). */
int lstat(const char *path, struct stat *st) {
    return stat(path, st);
}

/* The metadata of an open fd: one NP_FSTAT on its fid, decoded as stat's. The
 * console fds 0 to 2 answer a character device without a request. */
int fstat(int fd, struct stat *st) {
    if (!st) {
        return client_fail(EFAULT);
    }
    /* The console fds are valid and have no file behind them: a character
     * device, as a terminal is, rather than EBADF for a descriptor that
     * exists (the review of #220). */
    if (fd >= 0 && fd < FID_BASE) {
        memset(st, 0, sizeof *st);
        st->st_mode = S_IFCHR | 0666;
        return 0;
    }
    struct file_state *f = file_for(fd);
    if (!f) {
        return client_fail(EBADF);
    }
    unsigned char info[STAT_INFO_LEN] = {0}; /* zeroed: see stat_from_record */
    long s = np_request(f->target, f->endpoint, NP_FSTAT, f->fid, 0, 0, 0, 0, info, sizeof(info));
    if ((unsigned long)s >= FS_ERR_MIN) {
        set_errno_from_status();
        return -1;
    }
    stat_from_record(st, info);
    return 0;
}

/* POSIX: EBADF for a bad fd, ESPIPE for the console fds, EINVAL for an
 * unknown `whence` or a result before the start of the file, EOVERFLOW for
 * one past LONG_MAX; a refusal leaves the offset where it was. A `whence`
 * other than the three used to be taken as SEEK_SET. */
long lseek(int fd, long offset, int whence) {
    if (fd >= 0 && fd < FID_BASE) {
        return client_fail(ESPIPE);
    }
    struct file_state *f = file_for(fd);
    if (!f) {
        return client_fail(EBADF);
    }
    long base = 0;
    if (whence == SEEK_CUR) {
        base = f->offset;
    } else if (whence == SEEK_END) {
        struct stat s;
        if (fstat(fd, &s) < 0) {
            return -1;
        }
        base = s.st_size;
    } else if (whence != SEEK_SET) {
        return client_fail(EINVAL);
    }
    /* base is never negative (an offset or a size), so only a positive
     * offset can overflow, and the test must come before the sum: a signed
     * overflow is undefined, and the compiler may drop a check written after
     * it (the review of #220). */
    if (offset > 0 && base > LONG_MAX - offset) {
        return client_fail(EOVERFLOW);
    }
    if (base + offset < 0) {
        return client_fail(EINVAL);
    }
    f->offset = base + offset;
    return f->offset;
}

/* ---- stdout routing (console vs pipe) ------------------------------------ */

static long stdout_target(void) {
    static long cached = -1;
    if (cached < 0) {
        cached = __os_syscall1(SYS_STDOUT_TARGET, 0);
    }
    return cached;
}

static void console_write(const unsigned char *buf, size_t n) {
    size_t off = 0;
    while (off < n) {
        size_t chunk = n - off;
        if (chunk > FS_DATA_MAX) {
            chunk = FS_DATA_MAX;
        }
        unsigned char req[NP_REQ_PAYLOAD + FS_DATA_MAX];
        for (unsigned i = 0; i < NP_REQ_PAYLOAD; i++) {
            req[i] = 0;
        }
        __wr_u64(req + 0, NP_WRITE_FILE);
        __wr_u64(req + 24, chunk);
        memcpy(req + NP_REQ_PAYLOAD, buf + off, chunk);
        unsigned char reply[MSG_MAX_LEN];
        long r = __os_syscall4(SYS_MSG_CALL, CON_TASK, (long)req, (long)(NP_REQ_PAYLOAD + chunk),
                               (long)reply);
        if ((unsigned long)r >= FS_ERR_MIN) {
            for (size_t i = 0; i < chunk; i++) {
                __os_syscall1(SYS_PUTC, buf[off + i]);
            }
        }
        off += chunk;
    }
}

/* Send bytes to a pipe consumer via MSG_SEND. Yields on a full mailbox (rather
 * than dropping bytes) and retries a not-yet-delegated send - the same bounded
 * retry as ulib::pipe_out. */
static void pipe_write(long target, const unsigned char *buf, size_t n) {
    size_t off = 0;
    while (off < n) {
        size_t chunk = n - off;
        if (chunk > MSG_MAX_LEN) {
            chunk = MSG_MAX_LEN;
        }
        long deadline = __os_syscall1(SYS_GET_TICKS, 0) + 150;
        for (;;) {
            long r = __os_syscall4(SYS_MSG_SEND, target, (long)(buf + off), (long)chunk, 0);
            if (r == 0) {
                break;
            }
            unsigned long u = (unsigned long)r;
            int transient = (u == MSG_ERR_FULL || u == MSG_ERR_DENIED);
            if (!transient || __os_syscall1(SYS_GET_TICKS, 0) > deadline) {
                return;
            }
            if (u == MSG_ERR_FULL) {
                __os_syscall1(SYS_YIELD, 0);
            }
        }
        off += chunk;
    }
}

/* ---- stdout buffering ----------------------------------------------------
 *
 * Every write(1) is one IPC round trip - an MSG_CALL to the console server, or
 * an MSG_SEND to a pipe consumer. An unbuffered stdio therefore costs one round
 * trip PER CHARACTER, which is what picolibc's posix-console stdio does (our
 * own stdio.c buffers, so the hand-rolled libc never felt it). Buffering here,
 * at the write boundary rather than in one stdio, fixes it for whichever C
 * library is linked - and for a program calling write(1, ...) directly.
 *
 * LINE buffered, not fully buffered: a flush per line keeps output interactive
 * and matches the line-oriented filters on the other end of a pipe, while still
 * collapsing a per-character printf into one message per line.
 *
 * Three things are deliberately NOT buffered, because buffering them would
 * change observable behaviour rather than just batch it:
 *   - fd 2 (stderr) writes straight through, after flushing fd 1, so a message
 *     printed just before a crash is actually out, and in order.
 *   - a read from fd 0 flushes first, so a prompt without a trailing newline
 *     appears before the program waits for the answer (the stdin/stdout tie).
 *   - exit flushes, via _exit, which is the one path EVERY libc's exit reaches.
 */
#define OUT_BUF_SIZE 512
static unsigned char g_out[OUT_BUF_SIZE];
static size_t g_out_len;

/* Push whatever is buffered to the real destination. */
static void out_flush(void) {
    if (g_out_len == 0) {
        return;
    }
    size_t n = g_out_len;
    g_out_len = 0; /* cleared first: the write below must not re-enter this */
    long t = stdout_target();
    if (t == CON_TASK) {
        console_write(g_out, n);
    } else {
        pipe_write(t, g_out, n);
    }
}

/* Buffer a run of bytes destined for fd 1, flushing on a newline or a full
 * buffer. A line longer than the buffer is flushed in buffer-sized pieces. */
static void out_buffer(const unsigned char *buf, size_t n) {
    for (size_t i = 0; i < n; i++) {
        g_out[g_out_len++] = buf[i];
        if (buf[i] == '\n' || g_out_len == OUT_BUF_SIZE) {
            out_flush();
        }
    }
}

void __libc_end_stdout(void) {
    /* Idempotent: the hand-rolled libc's exit() calls this, and _exit calls it
     * again for the picolibc path where that exit() is not linked. */
    static int ended;
    out_flush(); /* the end-of-stream marker must not overtake the data */
    if (ended) {
        return;
    }
    ended = 1;
    long t = stdout_target();
    if (t == CON_TASK) {
        return;
    }
    unsigned char dummy = 0;
    __os_syscall4(SYS_MSG_SEND, t, (long)&dummy, 0, 0);
}

/* ---- ioctl ---------------------------------------------------------------- */

/* TIOCGWINSZ only: the console's size (sys/ioctl.h). fd 0 is the console's
 * keyboard; fds 1 and 2 are the console only while stdout is routed there,
 * and a pipe is not a terminal. */
int ioctl(int fd, unsigned long request, ...) {
    if (fd < 0) {
        return client_fail(EBADF);
    }
    int console = fd == 0 || ((fd == 1 || fd == 2) && stdout_target() == CON_TASK);
    if (!console) {
        if (fd > 2 && !file_for(fd)) {
            return client_fail(EBADF);
        }
        return client_fail(ENOTTY);
    }
    if (request != TIOCGWINSZ) {
        return client_fail(ENOTTY);
    }
    va_list ap;
    va_start(ap, request);
    struct winsize *ws = va_arg(ap, struct winsize *);
    va_end(ap);
    if (!ws) {
        return client_fail(EFAULT);
    }
    long cols = __os_syscall1(SYS_CON_INFO, CON_INFO_COLS);
    long rows = __os_syscall1(SYS_CON_INFO, CON_INFO_ROWS);
    /* 0 is "unknown" (a byte-stream console). A refusal is not: CON_INFO is
     * open to every task, so one means the kernel broke that, and saying
     * "unknown" would hide it. */
    if ((unsigned long)cols >= FS_ERR_MIN || (unsigned long)rows >= FS_ERR_MIN) {
        return client_fail(EIO);
    }
    if (cols > 0xffff || rows > 0xffff) {
        cols = 0;
        rows = 0;
    }
    ws->ws_row = (unsigned short)rows;
    ws->ws_col = (unsigned short)cols;
    ws->ws_xpixel = 0;
    ws->ws_ypixel = 0;
    return 0;
}

/* ---- read / write -------------------------------------------------------- */

ssize_t write(int fd, const void *buf, size_t count) {
    const unsigned char *p = (const unsigned char *)buf;
    if (fd == 1) {
        out_buffer(p, count);
        return (ssize_t)count;
    }
    if (fd == 2) {
        out_flush(); /* keep stderr in order with anything already buffered */
        long t = stdout_target();
        if (t == CON_TASK) {
            console_write(p, count);
        } else {
            pipe_write(t, p, count);
        }
        return (ssize_t)count;
    }
    struct file_state *f = file_for(fd);
    if (!f) {
        return client_fail(EBADF);
    }
    /* A REMOTE write and a LOCAL write differ only in how the data reaches
     * fsd: a LOCAL fid grants the buffer to fsd (which SAFECOPYs from it, so
     * the payload is empty), a REMOTE fid sends the data INLINE in the request
     * because NO GRANT CROSSES A MACHINE - netd's export bridges the inline
     * bytes to the far fsd via its own GRANT_READ (step 7 of
     * docs/roadmap/roadmap-fid-verbs.md; the export's fsd_pwrite). The remote
     * refusal that stood here until NP_PWRITE was served is gone. */
    int remote = ((f->target & 0xff) == NS_TARGET_REMOTE);
    size_t off = 0;
    while (off < count) {
        size_t chunk = count - off;
        if (chunk > FS_DATA_MAX) {
            chunk = FS_DATA_MAX;
        }
        long st;
        if (remote) {
            st = np_request(f->target, f->endpoint, NP_PWRITE, f->fid,
                            (unsigned long)f->offset, chunk,
                            (const char *)(p + off), chunk, 0, 0);
        } else {
            __os_syscall4(SYS_GRANT, FSD_TASK, (long)(p + off), (long)chunk, GRANT_READ);
            st = np_request(f->target, f->endpoint, NP_PWRITE, f->fid,
                            (unsigned long)f->offset, chunk, 0, 0, 0, 0);
        }
        if ((unsigned long)st >= FS_ERR_MIN) {
            if (off > 0) {
                return (ssize_t)off; /* POSIX: the short count, errno untouched */
            }
            set_errno_mode((f->flags & 3) != O_RDONLY);
            return -1;
        }
        /* Advance by what was ACTUALLY written (fsd's NP_PWRITE status is the
         * byte count), not by the whole chunk. A short or zero write (the far
         * fsd wrote fewer bytes than asked) then reports the true total rather
         * than a full success over a data gap, and st == 0 cannot spin the
         * loop. POSIX write() may return less than count; the caller retries. */
        f->offset += st;
        off += (size_t)st;
        if ((size_t)st < chunk) {
            break;
        }
    }
    return (ssize_t)off;
}

ssize_t read(int fd, void *buf, size_t count) {
    if (fd == 0) {
        if (count == 0) {
            return 0;
        }
        /* The stdin/stdout tie: show the prompt before waiting for the answer. */
        out_flush();
        long c = __os_syscall1(SYS_READ_CHAR, 0);
        ((unsigned char *)buf)[0] = (unsigned char)c;
        return 1;
    }
    struct file_state *f = file_for(fd);
    if (!f) {
        return client_fail(EBADF);
    }
    unsigned char *p = (unsigned char *)buf;
    size_t got = 0;
    while (got < count) {
        size_t want = count - got;
        if (want > FS_DATA_MAX) {
            want = FS_DATA_MAX;
        }
        long n = np_request(f->target, f->endpoint, NP_PREAD, f->fid,
                            (unsigned long)f->offset, want, 0, 0, p + got, want);
        if ((unsigned long)n >= FS_ERR_MIN) {
            if (got > 0) {
                return (ssize_t)got; /* POSIX: the short count, errno untouched */
            }
            set_errno_mode((f->flags & 3) != O_WRONLY);
            return -1;
        }
        if (n == 0) {
            break; /* EOF */
        }
        f->offset += n;
        got += (size_t)n;
        if ((size_t)n < want) {
            break;
        }
    }
    return (ssize_t)got;
}
