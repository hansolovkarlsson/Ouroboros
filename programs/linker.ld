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
 * .data/.bss are now supported (the loader's .data/.bss milestone): the
 * loader copies initialized data (p_filesz) and zeroes .bss (p_memsz -
 * p_filesz) per PT_LOAD segment, in both the boot-load and runtime-spawn
 * paths, so userland programs may have mutable statics/globals. The old
 * ASSERTs that rejected them are gone (see the note where they were). C
 * programs in particular need this - a C global or file-scope array is
 * .data/.bss - see docs/processes.md's "Writing a program in C".
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

    /* Initialized mutable statics/globals. Loaded like any other PT_LOAD
     * content - loader.rs copies p_filesz bytes to region_base + p_vaddr - and
     * relocated (a global holding a pointer emits an R_AARCH64_RELATIVE). Named
     * explicitly (not swept into an orphan default) so its placement is
     * deterministic. Supported since the loader's .data/.bss milestone; the old
     * ASSERT(SIZEOF(.data)==0) guard is gone. */
    .data : {
        *(.data .data.*)
    }

    /* Zero-initialized statics/globals. NOLOAD (no file bytes); the RW segment's
     * p_memsz exceeds its p_filesz by exactly this, and loader.rs zeroes that
     * tail (write_bytes 0). Supported since the loader's .data/.bss milestone. */
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

/* The ASSERTs that used to reject any non-empty .data/.bss are gone: the loader
 * now loads initialized data (p_filesz) and zeroes .bss (p_memsz - p_filesz) for
 * every PT_LOAD segment, in both the boot-load and runtime-spawn paths. Userland
 * programs may now have mutable statics/globals. (This does NOT lift the
 * separate liballoc-collections ceiling for Rust programs - those still fail to
 * link on an R_AARCH64_ABS64 in prebuilt liballoc, a different constraint.) */
