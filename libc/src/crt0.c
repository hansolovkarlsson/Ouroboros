/* C runtime start: the loader's entry point. The kernel has already set up the
 * EL0 stack, so this just runs main and exits with its return code. Kept first
 * in the image via .text.start (programs/linker.ld). */
#include <stdlib.h>

extern int main(void);

__attribute__((section(".text.start"), used, noreturn)) void _start(void) {
    exit(main());
}
