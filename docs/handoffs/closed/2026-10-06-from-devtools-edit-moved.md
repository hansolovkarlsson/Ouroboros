# Edit's requests now come from DevTools

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-06
- **Kind:** notice
- **Status:** done

## What is asked

Nothing new. This says who now owns Edit's notes here.

Edit moved into DevTools (`~/Projects/DevTools`) on 2026-10-06, with its
code in `DevTools/edit/` and its records merged into DevTools's `docs/`.
DevTools is the one project for the Ouroboros development tools: the editor,
the C preprocessor, the C compiler and those to come.

So for Edit's notes in this inbox:

- `2026-10-05-from-edit-editor-console.md`, **accepted**, is now DevTools's
  request. Its **Blocks** line names `~/Projects/Edit/docs/ROADMAP.md`, "The
  port to Ouroboros"; that section is now in
  `~/Projects/DevTools/docs/ROADMAP.md`, under **Edit**, with the same
  heading. Its source paths (`src/screen.h`, `src/platform.h`) are now
  under `DevTools/edit/`.
- `closed/2026-10-05-from-edit-rename.md`, **done**: DevTools has read the
  reply, and its roadmap records that the safe save on Ouroboros now waits
  on nothing.

Replies and further notes about the editor go to DevTools's
`docs/handoffs/`, and come from DevTools.

## Why

`~/Projects/DevTools/docs/ROADMAP.md`, "Bring Edit in", records the move.
The merge is DevTools `9e75cc9`; the records were folded in at `d11f98d`.

## Reply

Noted 2026-10-06 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing to start.

Checked: DevTools has Edit at `edit/` (`9e75cc9`, records at `d11f98d`),
`edit/src/screen.h` and `edit/src/platform.h` are there, and its
`docs/ROADMAP.md` has "The port to Ouroboros" under Edit;
`~/Projects/Edit` no longer exists. `2026-10-05-from-edit-editor-console.md`
stays accepted, and its reply now records the move, the new path of its
**Blocks** section and of its sources. `docs/ROADMAP.md`'s "What Edit asks
of Ouroboros" says the requests are DevTools's and replies about the editor
go to DevTools's `docs/handoffs/`. `closed/2026-10-05-from-edit-rename.md`
is unchanged.
