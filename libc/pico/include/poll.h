#ifndef _POLL_H
#define _POLL_H

/* poll for the keyboard: fd 0, POLLIN, with a timeout, for a full-screen
 * program that waits a moment for the rest of an escape sequence or repeats
 * a command until a key is pressed (DevTools's note
 * docs/handoffs/closed/2026-10-08-from-devtools-edit-poll.md). libc/src/file.c.
 *
 * What it does: for each entry with fd 0 and POLLIN, a byte waiting for the
 * keyboard's owner makes revents POLLIN, and the read(0, ...) after it
 * returns that byte without blocking. The byte stays in the kernel's queue
 * until that read takes it, so a program that polls and exits without
 * reading leaves it for the shell. A timeout of -1 waits for a byte, 0 does
 * not wait, and a positive one waits that many milliseconds, blocked in the
 * kernel (KEY_WAIT_UNTIL) without spinning, to a tick's precision: the wait
 * ends at the first tick at or after the time, so up to about a tick (20 ms)
 * late, and later when a tick is delayed or another program is busy. A
 * kernel without KEY_WAIT_UNTIL makes poll fail with ENOSYS. Output buffered for
 * fd 1 by the C library (what write fills, not stdio's buffer above it,
 * which the program flushes as on Unix) is written before the wait, as
 * read(0, ...) does. Ctrl+C and Ctrl+\ act as they do for read: in cooked
 * mode either ends the program while it polls. A task that does not own the
 * keyboard is answered 0 by a timed poll, and waits for the keyboard in a
 * poll with -1.
 *
 * The other descriptors: fds 0, 1 and 2, the console, are always ready for
 * POLLOUT; a file on a filesystem, local or remote, is ready for POLLIN and
 * POLLOUT, as POSIX has regular files; a descriptor on any other target
 * would be POLLERR, rather than a "ready" whose read would block, but none
 * exists yet: open refuses the /net binding (ENOSYS), so a /net file (a TCP
 * connection) cannot be polled because it cannot be opened; a descriptor
 * that is not open is
 * POLLNVAL; a negative one is skipped. POLLRDNORM and POLLWRNORM are taken
 * as POLLIN and POLLOUT; the band flags are never ready. When any entry is
 * ready, fd 0 is looked at without waiting. With nothing to wait on, the
 * timeout is a sleep, blocked in the kernel (SLEEP_UNTIL), and -1 sleeps for
 * good, as POSIX has it (Ctrl+C or Ctrl+\ in the foreground still ends it).
 *
 * The values are Linux's. Picolibc-side (libc/pico/include), staged under
 * /include on the image. */

typedef unsigned long nfds_t;

struct pollfd {
    int fd;
    short events;
    short revents;
};

#define POLLIN 0x001
#define POLLPRI 0x002
#define POLLOUT 0x004
#define POLLERR 0x008
#define POLLHUP 0x010
#define POLLNVAL 0x020
#define POLLRDNORM 0x040
#define POLLRDBAND 0x080
#define POLLWRNORM 0x100
#define POLLWRBAND 0x200

int poll(struct pollfd *fds, nfds_t nfds, int timeout);

#endif
