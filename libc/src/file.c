/* C file I/O over the filesystem server (fsd), plus stdout-target routing so a
 * C program participates in pipes/redirection.
 *
 * A small fd table maps integer fds (>= 3) to a path + cursor. fsd is
 * path-per-op (no server-side handles yet), so each read/write re-sends the
 * path with the tracked offset. Scope of this first cut: tree 0 (the default
 * disk mount) - remote mounts / namespaces are a follow-up; relative paths are
 * resolved against the shell-delivered cwd (GET_CWD).
 *
 * fds 0/1/2 (stdin/stdout/stderr) aren't table entries: read(0) -> keyboard,
 * write(1|2) -> the task's stdout target (the console, or a pipe consumer). */
#include "sys.h"
#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define MAX_FDS 16
#define PATH_MAX_C 128

struct fd_entry {
    int used;
    int flags;
    long offset;
    char path[PATH_MAX_C];
};

static struct fd_entry g_fds[MAX_FDS];

/* ---- fsd request helpers ------------------------------------------------- */

/* Send a ninep request to fsd (tree 0) with a path payload; copy up to
 * `reply_cap` bytes of the reply's DATA (after the 8-byte status) into
 * `reply_data`. Returns fsd's status (a byte count, 0, or an FS_ERR_* code);
 * a failed MSG_CALL (no fsd) comes back as an error too. */
static long fsd_request(unsigned long verb, unsigned long p0, unsigned long p1, unsigned long p2,
                        const char *path, size_t pathlen, unsigned char *reply_data,
                        size_t reply_cap) {
    unsigned char req[NP_REQ_PAYLOAD + FS_DATA_MAX];
    for (unsigned i = 0; i < NP_REQ_PAYLOAD; i++) {
        req[i] = 0;
    }
    __wr_u64(req + 0, verb);
    /* tree at offset 8 stays 0 */
    __wr_u64(req + 16, p0);
    __wr_u64(req + 24, p1);
    __wr_u64(req + 32, p2);
    if (pathlen > FS_DATA_MAX) {
        pathlen = FS_DATA_MAX;
    }
    memcpy(req + NP_REQ_PAYLOAD, path, pathlen);

    unsigned char reply[MSG_MAX_LEN];
    long r = __os_syscall4(SYS_MSG_CALL, FSD_TASK, (long)req, (long)(NP_REQ_PAYLOAD + pathlen),
                           (long)reply);
    if ((unsigned long)r >= FS_ERR_MIN) {
        return (long)FS_ERR_MIN; /* the call itself failed */
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

/* Grant `data` to fsd and issue an NP_WRITE_AT at `offset` (bulk write path). */
static long fsd_write_at(const char *path, size_t pathlen, long offset, const void *data,
                         size_t len) {
    __os_syscall4(SYS_GRANT, FSD_TASK, (long)data, (long)len, GRANT_READ);
    return fsd_request(NP_WRITE_AT, pathlen, (unsigned long)offset, len, path, pathlen, 0, 0);
}

/* ---- path resolution ----------------------------------------------------- */

/* Resolve `path` into `out` (absolute). Relative paths are joined to the cwd. */
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
    if (o == 0 || out[o - 1] != '/') {
        if (o < PATH_MAX_C - 1) {
            out[o++] = '/';
        }
    }
    for (size_t i = 0; path[i] && o < PATH_MAX_C - 1; i++) {
        out[o++] = path[i];
    }
    out[o] = 0;
}

/* ---- open / close / lseek / fstat ---------------------------------------- */

int open(const char *path, int flags, ...) {
    int fd = -1;
    for (int i = 3; i < MAX_FDS; i++) {
        if (!g_fds[i].used) {
            fd = i;
            break;
        }
    }
    if (fd < 0) {
        return -1; /* out of descriptors */
    }
    struct fd_entry *e = &g_fds[fd];
    resolve_path(path, e->path);

    size_t plen = strlen(e->path);
    if (flags & O_CREAT) {
        /* Create/truncate: NP_WRITE_FILE with zero data makes an empty file. */
        long st = fsd_request(NP_WRITE_FILE, plen, 0, 0, e->path, plen, 0, 0);
        if ((unsigned long)st >= FS_ERR_MIN) {
            return -1;
        }
    } else {
        /* Reading: confirm it exists via stat. */
        unsigned char info[STAT_INFO_LEN];
        long st = fsd_request(NP_STAT, plen, 0, 0, e->path, plen, info, sizeof(info));
        if ((unsigned long)st >= FS_ERR_MIN) {
            return -1; /* no such file */
        }
    }
    e->used = 1;
    e->flags = flags;
    e->offset = (flags & O_APPEND) ? -1 : 0; /* -1: resolve to EOF on first write */
    return fd;
}

int close(int fd) {
    if (fd < 3 || fd >= MAX_FDS || !g_fds[fd].used) {
        return -1;
    }
    g_fds[fd].used = 0;
    return 0;
}

long lseek(int fd, long offset, int whence) {
    if (fd < 3 || fd >= MAX_FDS || !g_fds[fd].used) {
        return -1;
    }
    struct fd_entry *e = &g_fds[fd];
    long base = 0;
    if (whence == SEEK_CUR) {
        base = e->offset;
    } else if (whence == SEEK_END) {
        struct stat s;
        if (fstat(fd, &s) < 0) {
            return -1;
        }
        base = s.st_size;
    }
    e->offset = base + offset;
    return e->offset;
}

int fstat(int fd, struct stat *st) {
    if (fd < 3 || fd >= MAX_FDS || !g_fds[fd].used || !st) {
        return -1;
    }
    struct fd_entry *e = &g_fds[fd];
    size_t plen = strlen(e->path);
    unsigned char info[STAT_INFO_LEN];
    long s = fsd_request(NP_STAT, plen, 0, 0, e->path, plen, info, sizeof(info));
    if ((unsigned long)s >= FS_ERR_MIN) {
        return -1;
    }
    st->st_size = (long)__rd_u64(info + STAT_SIZE_OFF);
    st->st_mode = (unsigned)(info[STAT_MODE_OFF] | (info[STAT_MODE_OFF + 1] << 8));
    return 0;
}

/* ---- stdout routing (console vs pipe) ------------------------------------ */

static long stdout_target(void) {
    static long cached = -1;
    if (cached < 0) {
        cached = __os_syscall1(SYS_STDOUT_TARGET, 0);
    }
    return cached;
}

/* Batched console write via cond (NP_WRITE_FILE), falling back to PUTC. */
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
        __wr_u64(req + 24, chunk); /* data_len at a1 */
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

/* Send bytes to a pipe consumer via MSG_SEND, chunked by MSG_MAX_LEN. On a full
 * consumer mailbox (MSG_ERR_FULL) it yields so the consumer can drain rather
 * than dropping bytes; a not-yet-delegated send (MSG_ERR_DENIED) just retries -
 * the same bounded-retry shape as ulib::pipe_out. Getting this wrong drops
 * bytes and merges lines in the downstream filter. */
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
                return; /* consumer gone, or gave up waiting */
            }
            if (u == MSG_ERR_FULL) {
                __os_syscall1(SYS_YIELD, 0); /* let the consumer drain */
            }
        }
        off += chunk;
    }
}

/* Called by exit(): if stdout is piped, send the end-of-stream empty message. */
void __libc_end_stdout(void) {
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
    if (fd == 1 || fd == 2) {
        long t = stdout_target();
        if (t == CON_TASK) {
            console_write(p, count);
        } else {
            pipe_write(t, p, count);
        }
        return (ssize_t)count;
    }
    if (fd < 3 || fd >= MAX_FDS || !g_fds[fd].used) {
        return -1;
    }
    struct fd_entry *e = &g_fds[fd];
    if (e->offset < 0) { /* O_APPEND: start at EOF */
        struct stat s;
        e->offset = (fstat(fd, &s) == 0) ? s.st_size : 0;
    }
    size_t off = 0;
    while (off < count) {
        size_t chunk = count - off;
        if (chunk > FS_DATA_MAX) {
            chunk = FS_DATA_MAX; /* keep each grant within a safe bound */
        }
        long st = fsd_write_at(e->path, strlen(e->path), e->offset, p + off, chunk);
        if ((unsigned long)st >= FS_ERR_MIN) {
            return (off > 0) ? (ssize_t)off : -1;
        }
        e->offset += (long)chunk;
        off += chunk;
    }
    return (ssize_t)count;
}

ssize_t read(int fd, void *buf, size_t count) {
    if (fd == 0) {
        if (count == 0) {
            return 0;
        }
        long c = __os_syscall1(SYS_READ_CHAR, 0);
        ((unsigned char *)buf)[0] = (unsigned char)c;
        return 1;
    }
    if (fd < 3 || fd >= MAX_FDS || !g_fds[fd].used) {
        return -1;
    }
    struct fd_entry *e = &g_fds[fd];
    unsigned char *p = (unsigned char *)buf;
    size_t got = 0;
    while (got < count) {
        size_t want = count - got;
        if (want > FS_DATA_MAX) {
            want = FS_DATA_MAX;
        }
        size_t plen = strlen(e->path);
        long n = fsd_request(NP_READ_AT, plen, (unsigned long)e->offset, want, e->path, plen,
                             p + got, want);
        if ((unsigned long)n >= FS_ERR_MIN) {
            return (got > 0) ? (ssize_t)got : -1;
        }
        if (n == 0) {
            break; /* EOF */
        }
        e->offset += n;
        got += (size_t)n;
        if ((size_t)n < want) {
            break; /* short read = EOF reached */
        }
    }
    return (ssize_t)got;
}
