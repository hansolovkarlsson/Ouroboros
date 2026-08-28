/* The two compiler-rt 128-bit shift builtins picolibc's exact-float printf
 * (the ryu algorithm) references. clang lowers a variable-count 128-bit shift
 * to a call to these, so they can't be written with `<<`/`>>` on the 128-bit
 * value itself (that would recurse) - they split the value into two 64-bit
 * halves and shift those (native ops). macOS ships compiler-rt only as Mach-O,
 * unusable for our ELF link, so we carry these two ourselves. Layout matches
 * compiler-rt's twords for little-endian AArch64. */

typedef int ti_t __attribute__((mode(TI)));
typedef unsigned int uti_t __attribute__((mode(TI)));

typedef union {
    uti_t all;
    struct {
        unsigned long long low;
        unsigned long long high;
    } s;
} twords;

/* Arithmetic/logical left shift of a 128-bit value by b (0..127). */
ti_t __ashlti3(ti_t a, int b) {
    twords in, out;
    in.all = (uti_t)a;
    if (b == 0) {
        return a;
    }
    if (b & 64) {
        out.s.low = 0;
        out.s.high = in.s.low << (b - 64);
    } else {
        out.s.low = in.s.low << b;
        out.s.high = (in.s.high << b) | (in.s.low >> (64 - b));
    }
    return (ti_t)out.all;
}

/* Logical right shift of an unsigned 128-bit value by b (0..127). */
ti_t __lshrti3(ti_t a, int b) {
    twords in, out;
    in.all = (uti_t)a;
    if (b == 0) {
        return a;
    }
    if (b & 64) {
        out.s.high = 0;
        out.s.low = in.s.high >> (b - 64);
    } else {
        out.s.high = in.s.high >> b;
        out.s.low = (in.s.low >> b) | (in.s.high << (64 - b));
    }
    return (ti_t)out.all;
}
