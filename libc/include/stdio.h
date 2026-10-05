#ifndef _STDIO_H
#define _STDIO_H

int putchar(int c);
int puts(const char *s);
int getchar(void);
/* Minimal printf: %d/%i, %u, %x/%X, %c, %s, %%. No width/precision/floats yet. */
int printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
/* Renames a file, replacing an existing ordinary file at `newpath`; 0 or -1.
 * Defined in file.c, beside open. */
int rename(const char *oldpath, const char *newpath);
/* A file (unlink) or an empty directory (rmdir); 0 or -1. */
int remove(const char *path);

#endif
