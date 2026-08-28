#ifndef _STDIO_H
#define _STDIO_H

int putchar(int c);
int puts(const char *s);
int getchar(void);
/* Minimal printf: %d/%i, %u, %x/%X, %c, %s, %%. No width/precision/floats yet. */
int printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

#endif
