/* Self-relocating (position-independent) linker script for Ouroboros
 * userland programs.
 *
 * Loaded and run at EL0 by the kernel (`loader.rs`/`tasks.rs`) - a real,
 * if narrowly-scoped, ELF loader now: it walks PT_LOAD program headers to
 * know what to copy where, and processes R_AARCH64_RELATIVE entries in
 * .rela.dyn against wherever it actually placed the program (see
 * loader.rs's own module doc comment for the full mechanism, and
 * CLAUDE.md's "relocating loader" milestone for why this replaced the
 * old flat-binary approach). Entry is still deliberately kept at the
 * very first byte of output (`KEEP(*(.text.start))`, link address 0x0)
 * even though a real ELF's entry point doesn't have to be - it keeps
 * `LoadedProgram::entry == LoadedProgram::base` trivially true, one
 * fewer thing to get wrong.
 *
 * Still no .bss/.data support (see the ASSERTs below) - the loader's own
 * segment-loading code is now generic enough to zero a real .bss region
 * (p_memsz - p_filesz per PT_LOAD segment), but *allowing* userland
 * static mutable state is a separate, deliberately-deferred capability
 * expansion, not a side effect of fixing the relocation bug - see
 * docs/processes.md's "Known rough edges".
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

    /* Populated now (empty and unused under the old relocation-model=static
     * build) - R_AARCH64_RELATIVE self-relocations, the actual mechanism
     * this whole change is for. loader.rs locates this by name via the
     * section header table (not stripped anymore - see Makefile's
     * shell-bin target), not by walking .dynamic. */
    .rela.dyn : {
        *(.rela.dyn)
    }

    /* Required by LLD to produce a well-formed ET_DYN/PT_DYNAMIC even
     * though loader.rs never reads either - there are no imported symbols
     * to resolve (--no-dynamic-linker, no shared libraries), so nothing
     * in .dynsym/.dynamic is semantically needed here, only structurally. */
    .dynsym : {
        *(.dynsym)
    }

    .dynamic : {
        *(.dynamic)
    }

    /* Position-independent codegen legitimately produces read-only-after-
     * relocation data (e.g. panic-location tracking for implicit bounds
     * checks, or core::fmt's argument-dispatch tables if a program uses
     * them) that the compiler places in .data.rel.ro, not .rodata, purely
     * because it contains pointers needing a relocation - it is not
     * "mutable state" in the sense the ASSERT below cares about. Routed
     * to its own output section (confirmed necessary by direct
     * experiment: without this, such content silently swept into .data
     * via the wildcard below and spuriously tripped the "no static
     * mutable state" ASSERT even for programs with none). Still copied
     * and relocated exactly like any other PT_LOAD segment content -
     * loader.rs's segment loading is section-name-agnostic.
     */
    .data.rel.ro : {
        *(.data.rel.ro .data.rel.ro.*)
    }

    /* Defined (not discarded) so an unexpected non-empty .data/.bss fails
     * loudly via the ASSERT below rather than silently landing in an
     * orphan-section default location. */
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
