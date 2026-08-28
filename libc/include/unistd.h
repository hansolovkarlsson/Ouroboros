#ifndef _UNISTD_H
#define _UNISTD_H
#include <stddef.h>

typedef long ssize_t;

/* Everything writes to the console for now (fd ignored); a stdout-target-aware
 * write() that participates in pipes/redirection is a follow-up. read() pulls
 * one blocking char from the keyboard (stdin). */
ssize_t write(int fd, const void *buf, size_t count);
ssize_t read(int fd, void *buf, size_t count);
void *sbrk(long incr);
void _exit(int code) __attribute__((noreturn));

#endif
