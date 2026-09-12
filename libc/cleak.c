/* Leak a fid ON PURPOSE: open a file, then leave through the raw EXIT syscall,
 * skipping _exit's close-all, so fsd is never told the fid is dead. This is the
 * check for fsd's fid reaper (`reap_dead_fids` in programs/servers/fsd/src/
 * main.rs), and it is a program because the condition it needs cannot be made
 * from the shell: the shell runs every foreground command in the SAME task
 * slot, so MAX_FIDS + 1 runs of this in a row leave MAX_FIDS leaked fids in
 * fsd's table, each owned by an earlier generation of the one slot, and the
 * last run's open succeeds only if the reaper frees by identity. A reaper that
 * asks whether the owning slot is alive finds that it is (the last run is in
 * it) and frees nothing, and that run's open fails - the fault this exists to
 * catch. Built by `make cleak-bin`, staged as /bin/CLEAK. Not a demo: a
 * program that does this by accident is the bug the reaper is for. */
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
#include "sys.h"

/* The two halves of _exit this program keeps, so its line reaches the console
 * (and a pipe consumer sees end-of-stream), minus the third, __libc_close_all. */
extern void __libc_flush_stdout(void);
extern void __libc_end_stdout(void);

int main(void) {
    int fd = open("/CLEAK.TXT", O_WRONLY | O_CREAT);
    if (fd < 0) {
        printf("cleak: open failed\r\n");
    } else {
        printf("cleak: opened fd %d and leaked it\r\n", fd);
    }
    __libc_flush_stdout();
    __libc_end_stdout();
    __os_syscall1(SYS_EXIT, fd < 0 ? 1 : 0);
    for (;;) {
    }
}
