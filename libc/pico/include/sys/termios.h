#ifndef _SYS_TERMIOS_H
#define _SYS_TERMIOS_H

/* Terminal settings for the console: tcgetattr and tcsetattr, for a
 * full-screen program that takes Ctrl+C as a key (DevTools's editor note,
 * item 4; step 2 of docs/roadmap/roadmap-ctrl-c.md). libc/src/file.c.
 *
 * ONE FLAG ACTS: ISIG. Cleared, the program is in raw keyboard mode (the
 * KBD_MODE syscall) and reads Ctrl+C as the byte 3; set (the default), Ctrl+C
 * ends it. Ctrl+\ ends a foreground program in EVERY mode and is never a key:
 * it is the way out no program can take away, which is the one difference
 * from Unix, where clearing ISIG makes ^\ a key too.
 *
 * Every other flag and c_cc entry is accepted and changes nothing, because
 * the console behaves one fixed way, and tcgetattr reports that way (with
 * ISIG as the kernel has it), not what was last set: POSIX has a program
 * confirm with tcgetattr what its tcsetattr took effect, and reading back a
 * stored ECHO would say echo is on when it is not. The fixed way:
 *   - ICANON, ECHO, ECHOE, ECHOK, ECHONL, IEXTEN: input is a byte at a time,
 *     with no echo and no line editing in the kernel (programs do their own).
 *   - ICRNL, INLCR, IGNCR: Enter arrives as 13 and nothing is translated.
 *   - IXON, IXOFF, IXANY: there is no flow control.
 *   - OPOST, ONLCR: output is written as it is; write "\r\n" for a new line.
 *   - BRKINT, INPCK, ISTRIP, PARMRK, CSIZE, CS8, PARENB, CREAD, CLOCAL: no
 *     serial line settings reach the hardware.
 *   - VINTR and VQUIT: the keys are fixed at Ctrl+C (3) and Ctrl+\ (28).
 *   - VMIN and VTIME: a read of fd 0 blocks for one byte and returns it.
 *   - VERASE: reported as 0x7f, what a serial terminal's Backspace sends;
 *     the USB keyboard's Backspace sends 8, so a program should take both.
 * So Edit's raw recipe, which clears flags the console already lacks, reads
 * back exactly as it was set, and saving and restoring the settings works.
 *
 * tcgetattr and tcsetattr answer ENOSYS on a kernel without KBD_MODE, and
 * tcsetattr EINVAL if the kernel refuses the mode.
 *
 * TCSANOW and TCSADRAIN are the same (output is never held back). TCSAFLUSH
 * does not discard pending input: the kernel's keyboard queue has no flush a
 * program can ask for, so keys typed ahead are kept, as with TCSADRAIN.
 *
 * Only the console is a terminal: fd 0, and fds 1 and 2 while stdout goes to
 * the console. Any other descriptor is ENOTTY (EBADF if it is not open), as
 * ioctl's TIOCGWINSZ is (sys/ioctl.h). Speed (cfgetospeed and its kin),
 * tcflush, tcdrain and the rest of <termios.h> are not provided: a program
 * that calls them fails to link rather than calling something untested.
 *
 * The values are Linux's, so a program that hard-codes one still works.
 * Picolibc-side (libc/pico/include): picolibc's <termios.h> includes this
 * file, and it is staged under /include on the image. */

typedef unsigned int tcflag_t;
typedef unsigned char cc_t;

#define NCCS 32

struct termios {
    tcflag_t c_iflag;
    tcflag_t c_oflag;
    tcflag_t c_cflag;
    tcflag_t c_lflag;
    cc_t c_line;
    cc_t c_cc[NCCS];
};

/* c_iflag */
#define IGNBRK 0000001
#define BRKINT 0000002
#define IGNPAR 0000004
#define PARMRK 0000010
#define INPCK 0000020
#define ISTRIP 0000040
#define INLCR 0000100
#define IGNCR 0000200
#define ICRNL 0000400
#define IXON 0002000
#define IXANY 0004000
#define IXOFF 0010000
#define IMAXBEL 0020000
#define IUTF8 0040000

/* c_oflag */
#define OPOST 0000001
#define ONLCR 0000004
#define OCRNL 0000010
#define ONOCR 0000020
#define ONLRET 0000040

/* c_cflag */
#define CSIZE 0000060
#define CS5 0000000
#define CS6 0000020
#define CS7 0000040
#define CS8 0000060
#define CSTOPB 0000100
#define CREAD 0000200
#define PARENB 0000400
#define PARODD 0001000
#define HUPCL 0002000
#define CLOCAL 0004000

/* c_lflag */
#define ISIG 0000001
#define ICANON 0000002
#define ECHO 0000010
#define ECHOE 0000020
#define ECHOK 0000040
#define ECHONL 0000100
#define NOFLSH 0000200
#define TOSTOP 0000400
#define IEXTEN 0100000

/* c_cc indices */
#define VINTR 0
#define VQUIT 1
#define VERASE 2
#define VKILL 3
#define VEOF 4
#define VTIME 5
#define VMIN 6
#define VSTART 8
#define VSTOP 9
#define VSUSP 10
#define VEOL 11

/* tcsetattr's optional_actions */
#define TCSANOW 0
#define TCSADRAIN 1
#define TCSAFLUSH 2

int tcgetattr(int fd, struct termios *t);
int tcsetattr(int fd, int optional_actions, const struct termios *t);

#endif
