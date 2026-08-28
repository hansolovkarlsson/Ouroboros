#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int putchar(int c) {
    char ch = (char)c;
    write(1, &ch, 1);
    return c;
}

int puts(const char *s) {
    write(1, s, strlen(s));
    putchar('\n');
    return 0;
}

int getchar(void) {
    unsigned char c;
    if (read(0, &c, 1) == 1) {
        return (int)c;
    }
    return -1;
}

static void print_uint(unsigned long v, int base, int upper) {
    char buf[24];
    int i = 0;
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    if (v == 0) {
        putchar('0');
        return;
    }
    while (v) {
        buf[i++] = digits[v % (unsigned)base];
        v /= (unsigned)base;
    }
    while (i > 0) {
        putchar(buf[--i]);
    }
}

static void print_int(long v) {
    if (v < 0) {
        putchar('-');
        print_uint((unsigned long)(-v), 10, 0);
    } else {
        print_uint((unsigned long)v, 10, 0);
    }
}

/* Minimal printf: %d/%i, %u, %x/%X, %c, %s, %%. No width, precision, or floats
 * yet - the subset a first round of C programs needs. */
int printf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') {
            putchar(*p);
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
            putchar((char)va_arg(ap, int));
            break;
        case 's': {
            const char *s = va_arg(ap, const char *);
            write(1, s, strlen(s));
            break;
        }
        case '%':
            putchar('%');
            break;
        case '\0':
            p--; /* trailing '%' - stop before the loop steps past NUL */
            break;
        default:
            putchar('%');
            putchar(*p);
            break;
        }
    }
    va_end(ap);
    return 0;
}
