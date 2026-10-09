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
 * wait, and a positive one waits at most that many milliseconds, measured
 * on the monotonic clock. Output buffered for fd 1 is written before the
 * wait, as read(0, ...) does, so what the program just drew is on the
 * screen while it waits. Ctrl+C and Ctrl+\ act as they do for read: in
 * cooked mode either ends the program while it polls.
 *
 * What it does not: any other descriptor (a file, a pipe, fd 1) is answered
 * POLLNVAL at once, and an entry with a negative fd is skipped, as POSIX
 * has it. A task that does not own the keyboard never sees POLLIN.
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
