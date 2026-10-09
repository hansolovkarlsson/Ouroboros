#ifndef _POLL_H
#define _POLL_H

/* poll for the keyboard: fd 0, POLLIN, with a timeout, for a full-screen
 * program that waits a moment for the rest of an escape sequence or repeats
 * a command until a key is pressed (DevTools's note
 * docs/handoffs/2026-10-08-from-devtools-edit-poll.md). libc/src/file.c.
 *
 * What it does: for each entry with fd 0 and POLLIN, a byte waiting for the
 * keyboard's owner makes revents POLLIN, and the read(0, ...) after it
 * returns that byte without blocking (poll takes the byte from the kernel
 * and holds it for the read). A timeout of -1 waits for a byte, 0 does not
 * wait, and a positive one waits at most that many milliseconds, blocked in
 * the kernel for all but the last tick (READ_CHAR_UNTIL) and to well within
 * a tick of the time. Output buffered for fd 1 by the C library (what write
 * fills, not stdio's buffer above it, which the program flushes as on Unix)
 * is written before the wait, as read(0, ...) does. Ctrl+C and Ctrl+\ act
 * as they do for read: in cooked mode either ends the program while it
 * polls. A task that does not own the keyboard is answered 0 by a timed
 * poll, and waits for the keyboard in a poll with -1, as read would.
 *
 * The other descriptors: fds 1 and 2 are always ready for POLLOUT; an open
 * file is ready for POLLIN and POLLOUT, as POSIX has regular files; a
 * descriptor that is not open is POLLNVAL; a negative one is skipped. When
 * any entry is ready, fd 0 is looked at without waiting. With nothing to wait
 * on, a positive timeout is a sleep (it looks on the clock, there being no
 * key to block on) and -1 is refused with EINVAL rather than hanging for good.
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

int poll(struct pollfd *fds, nfds_t nfds, int timeout);

#endif
