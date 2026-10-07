#ifndef _SYS_IOCTL_H
#define _SYS_IOCTL_H

/* The one ioctl Ouroboros has: TIOCGWINSZ, the console's size, for a
 * full-screen program (DevTools's editor note, item 3). libc/src/file.c.
 *
 * On a framebuffer console it answers the screen's character grid. On a
 * byte-stream console (QEMU's serial line) the size is unknown and it
 * answers 0 rows and 0 columns, as Linux does for a terminal whose size was
 * never set; a caller then assumes 80 by 24. Any descriptor that is not the
 * console (a pipe on stdout, an open file) is ENOTTY, and so is any other
 * request. */

struct winsize {
    unsigned short ws_row;
    unsigned short ws_col;
    unsigned short ws_xpixel; /* always 0 */
    unsigned short ws_ypixel; /* always 0 */
};

/* Linux's number, so a program that hard-codes it still works. */
#define TIOCGWINSZ 0x5413

int ioctl(int fd, unsigned long request, ...);

#endif
