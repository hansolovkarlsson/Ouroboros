#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* Buffered stdout. Without this, printf emitted one write() per character - a
 * flood of 1-byte pipe messages that overran the consumer's mailbox and dropped
 * bytes. Buffering flushes a whole line (or 256 bytes) at once. The buffer is a
 * .bss static (the .data/.bss milestone). Flushed on newline, when full, and at
 * program exit (via __libc_flush_stdout, called by exit before end-of-stream). */
#define OBUF_SIZE 256
static unsigned char g_obuf[OBUF_SIZE];
static int g_olen;

void __libc_flush_stdout(void) {
    if (g_olen > 0) {
        write(1, g_obuf, (size_t)g_olen);
        g_olen = 0;
    }
}

static void obuf_put(unsigned char c) {
    g_obuf[g_olen++] = c;
    if (c == '\n' || g_olen == OBUF_SIZE) {
        __libc_flush_stdout();
    }
}

int putchar(int c) {
    obuf_put((unsigned char)c);
    return c;
}

int puts(const char *s) {
    for (size_t i = 0; s[i]; i++) {
        obuf_put((unsigned char)s[i]);
    }
    obuf_put('\n');
    return 0;
}

int getchar(void) {
    unsigned char c;
    if (read(0, &c, 1) == 1) {
        return (int)c;
    }
    return -1;
}

static void print_str(const char *s) {
    for (size_t i = 0; s[i]; i++) {
        obuf_put((unsigned char)s[i]);
    }
}

static void print_uint(unsigned long v, int base, int upper) {
    char buf[24];
    int i = 0;
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    if (v == 0) {
        obuf_put('0');
        return;
    }
    while (v) {
        buf[i++] = digits[v % (unsigned)base];
        v /= (unsigned)base;
    }
    while (i > 0) {
        obuf_put((unsigned char)buf[--i]);
    }
}

static void print_int(long v) {
    if (v < 0) {
        obuf_put('-');
        print_uint((unsigned long)(-v), 10, 0);
    } else {
        print_uint((unsigned long)v, 10, 0);
    }
}

/* Minimal printf: %d/%i, %u, %x/%X, %c, %s, %%. No width/precision/floats yet. */
int printf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') {
            obuf_put((unsigned char)*p);
            continue;
        }
        p++;
        switch (*p) {
        case 'd':
        case 'i':
            print_int(va_arg(ap, int));
            break;
        case 'u':
            print_uint((unsigned long)va_arg(ap, unsigned int), 10, 0);
            break;
        case 'x':
            print_uint((unsigned long)va_arg(ap, unsigned int), 16, 0);
            break;
        case 'X':
            print_uint((unsigned long)va_arg(ap, unsigned int), 16, 1);
            break;
        case 'c':
            obuf_put((unsigned char)va_arg(ap, int));
            break;
        case 's':
            print_str(va_arg(ap, const char *));
            break;
        case '%':
            obuf_put('%');
            break;
        case '\0':
            p--;
            break;
        default:
            obuf_put('%');
            obuf_put((unsigned char)*p);
            break;
        }
    }
    va_end(ap);
    return 0;
}
