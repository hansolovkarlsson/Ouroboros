/* C file I/O over the filesystem server (fsd) using fids - server-side
 * open-file handles (a POSIX fd IS a 9P fid). open() establishes a fid in fsd
 * (which authorizes the access once, against the file's mode/owner), and the fd
 * a C program holds *is* that fid; read/write/stat/close reference it. The
 * cursor stays client-side and rides each read/write offset (authentic 9P).
 *
 * Also here: stdout-target routing, so write(1|2) goes to the console or, when
 * the program is a pipe producer, to the consumer - so a C program works in a
 * pipeline. Paths resolve against the shell-delivered cwd; tree 0 (the default
 * disk mount) only, for now. */
#include "sys.h"
#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define MAX_FILES 8 /* fsd's fid table size; fds run FID_BASE .. FID_BASE+7 */
#define PATH_MAX_C 96

/* Per-open client state: the cursor + flags. Indexed by (fd - FID_BASE); the fd
 * itself is the fsd fid. */
struct file_state {
    int used;
    long offset;
    int flags;
};
static struct file_state g_files[MAX_FILES];

/* ---- fsd request helper -------------------------------------------------- */

/* Send a ninep request to fsd (tree 0) with an optional path payload; copy up
 * to `reply_cap` bytes of the reply DATA (after the 8-byte status) into
 * `reply_data`. Returns fsd's status. */
static long fsd_request(unsigned long verb, unsigned long p0, unsigned long p1, unsigned long p2,
                        const char *path, size_t pathlen, unsigned char *reply_data,
                        size_t reply_cap) {
    unsigned char req[NP_REQ_PAYLOAD + FS_DATA_MAX];
    for (unsigned i = 0; i < NP_REQ_PAYLOAD; i++) {
        req[i] = 0;
    }
    __wr_u64(req + 0, verb);
    __wr_u64(req + 16, p0);
    __wr_u64(req + 24, p1);
    __wr_u64(req + 32, p2);
    if (pathlen > FS_DATA_MAX) {
        pathlen = FS_DATA_MAX;
    }
    if (pathlen && path) {
        memcpy(req + NP_REQ_PAYLOAD, path, pathlen);
    }
    unsigned char reply[MSG_MAX_LEN];
    long r = __os_syscall4(SYS_MSG_CALL, FSD_TASK, (long)req, (long)(NP_REQ_PAYLOAD + pathlen),
                           (long)reply);
    if ((unsigned long)r >= FS_ERR_MIN) {
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
    return (long)__rd_u64(reply);
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

    size_t plen = strlen(abspath);
    long fid = fsd_request(NP_OPEN, oflags, plen, 0, abspath, plen, 0, 0);
    if ((unsigned long)fid >= FS_ERR_MIN || fid < FID_BASE) {
        return -1;
    }
    int idx = (int)(fid - FID_BASE);
    if (idx < 0 || idx >= MAX_FILES) {
        return -1;
    }
    g_files[idx].used = 1;
    g_files[idx].flags = flags;
    g_files[idx].offset = 0;
    return (int)fid;
}

static struct file_state *file_for(int fd) {
    int idx = fd - FID_BASE;
    if (idx < 0 || idx >= MAX_FILES || !g_files[idx].used) {
        return 0;
    }
    return &g_files[idx];
}

int close(int fd) {
    struct file_state *f = file_for(fd);
    if (!f) {
        return -1;
    }
    fsd_request(NP_CLUNK, (unsigned long)fd, 0, 0, 0, 0, 0, 0);
    f->used = 0;
    return 0;
}

int fstat(int fd, struct stat *st) {
    struct file_state *f = file_for(fd);
    if (!f || !st) {
        return -1;
    }
    unsigned char info[STAT_INFO_LEN];
    long s = fsd_request(NP_FSTAT, (unsigned long)fd, 0, 0, 0, 0, info, sizeof(info));
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
    size_t off = 0;
    while (off < count) {
        size_t chunk = count - off;
        if (chunk > FS_DATA_MAX) {
            chunk = FS_DATA_MAX;
        }
        __os_syscall4(SYS_GRANT, FSD_TASK, (long)(p + off), (long)chunk, GRANT_READ);
        long st = fsd_request(NP_PWRITE, (unsigned long)fd, (unsigned long)f->offset, chunk, 0, 0, 0, 0);
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
        long n = fsd_request(NP_PREAD, (unsigned long)fd, (unsigned long)f->offset, want, 0, 0, p + got,
                             want);
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
