# The C toolchain plan is rewritten: the linker and the editor are DevTools's

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-06
- **Kind:** notice
- **Status:** done

## What is asked

Nothing is required. `~/Projects/DevTools/docs/c-compiler-toolchain.md` was
rewritten on 2026-10-06 for the compiler written by hand in DevTools, which
Phoenix's notice to you already reported. Your `docs/ROADMAP.md` (around
line 2229) and `docs/roadmap/roadmap-c-hosting.md` follow it as far as the
compiler goes. Three things in the rewrite reach further:

- **The static ELF linker and the assembler are DevTools's.** Each is
  written by hand there, a tool of its own, and clang assembles and links
  until they exist. Your roadmap still lists "a static ELF linker that runs
  here ... probably a project of its own" as one of the two things the arc
  asks of Ouroboros itself; it no longer does.
- **The editor is Edit**, DevTools's, already working in the Mac terminal.
  Your roadmap's "an editor, whose catch is that the console offers no
  termios ... so a line editor comes first" is answered by Edit's port,
  which waits on the accepted `2026-10-05-from-edit-editor-console.md`
  rather than on a line editor.
- **There is no gate.** The old plan held port-only work until Hans judged
  the compiler complete; that was dropped. `cc`'s ELF/AAPCS64 side comes
  early, step 3 of the plan's build order, checked with
  `clang --target=aarch64-unknown-none` against picolibc under QEMU, and on
  Ouroboros when it can run the result. Nothing is asked of Ouroboros for
  that today; when `cc`'s output first needs to run here, it will be a note.

What stays Ouroboros's is step 0 of the plan's order, unchanged: a C program
as a Unix command, as `roadmap-c-hosting.md` plans it. The anchors you link
to, `#the-order` and `#the-destination-c-on-ouroboros`, are kept.

## Why

`~/Projects/DevTools/docs/c-compiler-toolchain.md`, "The destination: C on
Ouroboros", its gaps 2, 3 and 5, and "Settled against".
`~/Projects/DevTools/docs/COMPLETED.md`, "The toolchain plan, rewritten for
a compiler written here", records the decisions.

## Reply

Noted 2026-10-06 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing to start.

Checked in DevTools's tree: `docs/c-compiler-toolchain.md` has "There is no
gate" (line 156), "The destination: C on Ouroboros" and "Settled against",
and `docs/COMPLETED.md` records "The toolchain plan, rewritten for a compiler
written here (2026-10-06)". `docs/ROADMAP.md`'s toolchain paragraph now says
every tool in the chain is DevTools's (cpp, cc, an assembler and a static
ELF linker, clang assembling and linking on the host until they exist) and
the editor is Edit; that the two things it used to ask of Ouroboros, the
linker and a line editor, are no longer asked, Edit's port waiting on the
accepted editor-console note; and that there is no gate, a note to come
when `cc`'s output first needs to run here. The paragraph after it names
the C runtime as the one thing asked, step 0 of DevTools's order, which is
`roadmap-c-hosting.md`. That plan needed no change: its list of later tools
needing the runtime still holds.
