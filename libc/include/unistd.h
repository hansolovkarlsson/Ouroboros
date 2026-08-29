#ifndef _UNISTD_H
#define _UNISTD_H
#include <stddef.h>

typedef long ssize_t;

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

/* write(1|2) -> the task's stdout target (the console, or a pipe consumer);
 * write(fd>=3) -> the file. read(0) <- keyboard (stdin); read(fd>=3) <- file. */
ssize_t write(int fd, const void *buf, size_t count);
ssize_t read(int fd, void *buf, size_t count);
int close(int fd);
long lseek(int fd, long offset, int whence);
void *sbrk(long incr);
void _exit(int code) __attribute__((noreturn));

/* Internal. __libc_flush_stdout drains our own stdio.c's buffer (the
 * hand-rolled libc only). __libc_end_stdout flushes the write-level stdout
 * buffer and then, if stdout is piped, sends the end-of-stream marker; it is
 * idempotent and _exit calls it, so it runs for a picolibc program too - that
 * build links picolibc's exit(), not our stdlib.c's. */
void __libc_flush_stdout(void);
/* Close every open fd - called by _exit, because a fid is server-side state
 * that nothing else releases. */
void __libc_close_all(void);
void __libc_end_stdout(void);

#endif
