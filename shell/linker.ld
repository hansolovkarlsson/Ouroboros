/* Flat-binary linker script for Ouroboros userland programs.
 *
 * Loaded and run at EL0 by the kernel (tasks.rs), not by any OS loader of
 * our own devising - there is no ELF interpreter, no relocation processing,
 * nothing beyond "read these bytes into a page-aligned buffer and jump to
 * offset 0". That constrains this script: entry must be the very first
 * byte of output, and there must be nothing here a raw `objcopy -O binary`
 * can't represent (in particular, no .bss - see main.rs's module doc
 * comment for why this program deliberately has no static mutable state).
 */

ENTRY(_start)

SECTIONS
{
    . = 0x0;

    .text : {
        KEEP(*(.text.start))
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    /* Defined (not discarded) so an unexpected non-empty .data/.bss fails
     * loudly via the ASSERT below rather than silently landing in an
     * orphan-section default location that objcopy might not capture. */
    .data : {
        *(.data .data.*)
    }

    .bss (NOLOAD) : {
        *(.bss .bss.*)
    }

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.ARM.exidx*)
        *(.ARM.extab*)
        *(.note*)
    }
}

ASSERT(SIZEOF(.data) == 0, "userland program has non-empty .data - no loader support for initialized statics yet")
ASSERT(SIZEOF(.bss) == 0, "userland program has non-empty .bss - no loader support for zeroed statics yet")
