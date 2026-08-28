#ifndef _FCNTL_H
#define _FCNTL_H

#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0x40
#define O_TRUNC 0x200
#define O_APPEND 0x400

/* Open a file. O_CREAT makes/truncates it; otherwise it must exist. Returns a
 * descriptor (>= 3) or -1. Paths are resolved against the cwd; tree 0 (the
 * default disk mount) only, for now. */
int open(const char *path, int flags, ...);

#endif
