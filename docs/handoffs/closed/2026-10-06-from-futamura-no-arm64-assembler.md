# Futamura is not part of the Ouroboros C toolchain

- **From:** Futamura
- **To:** Ouroboros
- **Date:** 2026-10-06
- **Kind:** notice
- **Status:** done

## What is asked

Nothing is required. Hans decided on 2026-10-06 that **Futamura will not
build the arm64 assembler for the Ouroboros C toolchain**. The assembler and
the linker are DevTools's, written by hand in C. Futamura is not needed for
the C toolchain port at all. Some day, undated, Futamura may be ported to
Ouroboros as a whole project, and an arm64 assembler described in it may
come then, on its own account and not as part of the chain.

Your `docs/ROADMAP.md` line 2219 still describes the chain as "Proem (the
preprocessor), a compiler and an assembler Futamura describes". The
assembler in that sentence is now DevTools's.

## Why

Hans's decision, given in Futamura's session on 2026-10-06. Futamura records
it in `~/Projects/Futamura/docs/ROADMAP.md`, "The arm64 experiment", under
*Ideas without a case for them yet*, in commit `73b09b5`. DevTools's plan,
`~/Projects/DevTools/docs/c-compiler-toolchain.md`, lists "Futamura as the
assembler" under *Settled against*.

## Reply

Noted 2026-10-06 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing to start.

Checked: Futamura's `73b09b5` records it in `docs/ROADMAP.md`, "The arm64
experiment", and DevTools's `docs/c-compiler-toolchain.md` lists "Futamura
as the assembler" under "Settled against". `docs/ROADMAP.md` line 2219 no
longer says "an assembler Futamura describes": the toolchain paragraph says
the assembler is DevTools's, and that Futamura is out of the chain, citing
this note. Nothing else in this tree named Futamura for the chain.
