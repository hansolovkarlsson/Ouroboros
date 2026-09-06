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
 * docs/roadmap-fid-verbs.md.
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
#include <sys/stat.h>
#include <unistd.h>

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

/* The status of the last failed request. open()/read()/write() return -1 with
 * no errno (this libc has none), so without this a C program cannot tell "no
 * such file" from "that server does not implement this request" - the exact
 * confusion FS_ERR_NO_SUCH_VERB was reserved to end, stopping one layer short
 * of C. Not errno: one value, no thread story, no claim to be more.
 *
 * CLEARED ON SUCCESS, or a later failure that sets nothing reports whatever the
 * previous call left behind - a stale answer being worse than none here. */
static unsigned long g_last_status;

/* A failure that never reached a server: a path too long to resolve, or no free
 * fd slot. NOT FS_ERR_MIN, which this used and which IS FS_ERR_NO_SUCH_VERB -
 * they are the same u64::MAX - 39, so a client-side failure reported itself as
 * "that server does not implement this request", the precise confusion that
 * code was reserved to end. Distinct, and deliberately not a wire value. */
#define FS_ERR_CLIENT (~0UL - 1UL - 39UL)

unsigned long ouro_last_fs_status(void) {
    return g_last_status;
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
    long r = __os_syscall4(SYS_MSG_CALL, dest, (long)req,
                           (long)(base + NP_REQ_PAYLOAD + pathlen), (long)reply);
    if ((unsigned long)r >= FS_ERR_MIN) {
        /* A transport failure, not a server answer - MSG_ERR_DENIED is the
         * likely one (netd's send capability is delegated after spawn). */
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
    if ((unsigned long)status >= FS_ERR_MIN) {
        g_last_status = (unsigned long)status;
    }
    return status;
}

/* ---- path resolution (for open) ------------------------------------------ */

static void resolve_path(const char *path, char *out) {
    if (path[0] == '/') {
        size_t n = strlen(path);
        if (n >= PATH_MAX_C) {
            n = PATH_MAX_C - 1;
        }
        memcpy(out, path, n);
        out[n] = 0;
        return;
    }
    char cwd[PATH_MAX_C];
    long clen = __os_syscall4(SYS_GET_CWD, (long)cwd, sizeof(cwd), 0, 0);
    if (clen <= 0 || clen >= PATH_MAX_C) {
        clen = 1;
        cwd[0] = '/';
    }
    size_t o = 0;
    for (long i = 0; i < clen && o < PATH_MAX_C - 1; i++) {
        out[o++] = cwd[i];
    }
    if ((o == 0 || out[o - 1] != '/') && o < PATH_MAX_C - 1) {
        out[o++] = '/';
    }
    for (size_t i = 0; path[i] && o < PATH_MAX_C - 1; i++) {
        out[o++] = path[i];
    }
    out[o] = 0;
}

/* ---- open / close / lseek / fstat ---------------------------------------- */

int open(const char *path, int flags, ...) {
    char abspath[PATH_MAX_C];
    resolve_path(path, abspath);

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
    unsigned long fslen = 0;
    unsigned target = 0;
    unsigned char endpoint[NS_ENDPOINT_LEN];
    memset(endpoint, 0, sizeof endpoint);
    if (ouro_ns_resolve(abspath, strlen(abspath), fspath, sizeof fspath, &fslen,
                        &target, endpoint) != 0) {
        g_last_status = FS_ERR_CLIENT;
        return -1;
    }
    /* The console and /net are not openable files: both are served by other
     * servers with no fid model at all, and answering "no such file" for them
     * would be the same lie this arc has been removing. */
    if ((target & 0xff) == NS_TARGET_CONSOLE || (target & 0xff) == NS_TARGET_NETLOCAL) {
        g_last_status = FS_ERR_NO_SUCH_VERB;
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
        g_last_status = FS_ERR_CLIENT;
        return -1;
    }

    /* NP_OPEN's a0 is the FLAGS and a1 the path length - the reverse of every
     * other path-carrying verb. Getting this backwards resolves a 1-3 byte path
     * out of the flag word and lands somewhere plausible instead of failing. */
    long fid = np_request(target, endpoint, NP_OPEN, oflags, fslen, 0,
                          fspath, (size_t)fslen, 0, 0);
    if ((unsigned long)fid >= FS_ERR_MIN || fid < FID_BASE) {
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

static struct file_state *file_for(int fd) {
    int idx = fd - FID_BASE;
    if (idx < 0 || idx >= MAX_FILES || !g_files[idx].used) {
        return 0;
    }
    return &g_files[idx];
}

/* Close every open fd at exit.
 *
 * A fid is server-side state in fsd, and nothing else releases it: fsd reaps a
 * fid only when its owner SLOT reads dead, but slots are recycled and the shell
 * reuses the same one for every foreground command - so a C program that relies
 * on exit to close its files (standard practice, and what picolibc's exit does)
 * leaks a fid permanently. Eight of those exhaust fsd's table and every
 * subsequent open fails, for every program, until fsd restarts. */
void __libc_close_all(void) {
    for (int i = 0; i < MAX_FILES; i++) {
        if (g_files[i].used) {
            close(i + FID_BASE);
        }
    }
}

int close(int fd) {
    struct file_state *f = file_for(fd);
    if (!f) {
        return -1;
    }
    np_request(f->target, f->endpoint, NP_CLUNK, f->fid, 0, 0, 0, 0, 0, 0);
    f->used = 0;
    return 0;
}

int fstat(int fd, struct stat *st) {
    struct file_state *f = file_for(fd);
    if (!f || !st) {
        return -1;
    }
    unsigned char info[STAT_INFO_LEN];
    long s = np_request(f->target, f->endpoint, NP_FSTAT, f->fid, 0, 0, 0, 0, info, sizeof(info));
    if ((unsigned long)s >= FS_ERR_MIN) {
        return -1;
    }
    st->st_size = (long)__rd_u64(info + STAT_SIZE_OFF);
    st->st_mode = (unsigned)(info[STAT_MODE_OFF] | (info[STAT_MODE_OFF + 1] << 8));
    return 0;
}

long lseek(int fd, long offset, int whence) {
    struct file_state *f = file_for(fd);
    if (!f) {
        return -1;
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
        return -1;
    }
    /* A REMOTE WRITE IS REFUSED, not attempted.
     *
     * The first version of this granted the buffer to netd and sent NP_PWRITE
     * with no payload - and NO GRANT CROSSES A MACHINE. netd's rmount relay
     * forwards the NP message verbatim and never looks at a grant, so the far
     * side would have received a bare 48-byte header, while this function
     * advanced the offset and returned `count`: a silent zero-byte write
     * reported as success. (The comment there claimed netd bridged the grant.
     * It confused the EXPORT side - fsd_write_at, which does bridge, inbound -
     * with the outbound relay, which does not.)
     *
     * The data must ride INLINE in the request, the way ulib::fs_write_at does
     * it. That wire shape is not defined for NP_PWRITE yet: no export
     * implements the verb (step 6 of docs/roadmap-fid-verbs.md), so there is
     * nothing to agree with and nothing to test against. Refusing is the
     * honest answer until there is - and it is checkable today, which a second
     * untested implementation would not be. */
    if ((f->target & 0xff) == NS_TARGET_REMOTE) {
        g_last_status = FS_ERR_NO_SUCH_VERB;
        return -1;
    }
    size_t off = 0;
    while (off < count) {
        size_t chunk = count - off;
        if (chunk > FS_DATA_MAX) {
            chunk = FS_DATA_MAX;
        }
        __os_syscall4(SYS_GRANT, FSD_TASK, (long)(p + off), (long)chunk, GRANT_READ);
        long st = np_request(f->target, f->endpoint, NP_PWRITE, f->fid,
                             (unsigned long)f->offset, chunk, 0, 0, 0, 0);
        if ((unsigned long)st >= FS_ERR_MIN) {
            return (off > 0) ? (ssize_t)off : -1;
        }
        f->offset += (long)chunk;
        off += chunk;
    }
    return (ssize_t)count;
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
        return -1;
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
            return (got > 0) ? (ssize_t)got : -1;
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
