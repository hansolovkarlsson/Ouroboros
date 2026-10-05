# `rename` in the C library

- **From:** Edit
- **To:** Ouroboros
- **Date:** 2026-10-05
- **Kind:** requirement
- **Status:** done
- **Blocks:** Edit saving safely on Ouroboros. Without it the port saves
  with `O_TRUNC`, which works but can leave a half-written file.
- **Follows:** `2026-10-05-from-edit-editor-console.md`, whose last
  paragraph wrongly said Proem had already asked for `rename`. Your reply
  corrected that, and this note is the request the reply pointed to.

## What is asked

`int rename(const char *oldpath, const char *newpath)` in the C port, on
top of `FSOP_MV`, with POSIX's meaning for the case Edit uses: when
`newpath` is an existing ordinary file, it is replaced. It returns 0, or
-1 on failure (with `errno` once there is one).

Edit's save writes the new text to `<file>.edit-tmp` beside the original,
then renames it over the original. The old file stays whole until the new
one is complete, so a failed write (a full disk, a lost USB stick) costs
the edit and not the file. That is the only use. Edit does not rename
directories or move files across mounts.

## Why

Read from the tree on branch `fsd/fat-cache` (`1e0ec24`), 2026-10-05:

- `third_party/picolibc-prebuilt/include/stdio.h:364` declares `rename`,
  but neither `libc.a` (`nm` shows no `rename` symbol) nor `libc/src/`
  (no match for `rename`) defines it, so a C program that calls it fails
  to link.
- The server side is already there. `FSOP_MV`
  (`syscall-abi/src/lib.rs:1283-1285`) renames, and `docs/ROADMAP.md`
  (around line 3601, "`mv` cannot replace an existing destination", fixed
  2026-09-02) records that it now replaces an existing ordinary file. On
  ext2 that is one directory-entry write. On FAT32 and exFAT it takes two,
  ordered so that a reader always finds one of the two files. That is what
  Edit needs: the name always resolves to a whole file.
- The doc comment on `FSOP_MV` (`syscall-abi/src/lib.rs:1283-1284`) still
  says "`dst` must not already exist", which the 2026-09-02 fix made out of
  date. That is mentioned only in case it misleads whoever writes the
  wrapper.
- Edit's side: `~/Projects/Edit/src/platform_posix.c`, `plat_write_file`,
  is the code the Ouroboros platform file will mirror.

## Done when

A C program built for Ouroboros links against `rename`, and on a FAT32
volume `rename("a.tmp", "a.txt")` with both files present returns 0 and
leaves `a.txt` holding what `a.tmp` held, and `a.tmp` gone.

## Reply

Accepted 2026-10-05 by Ouroboros, for later: on `docs/ROADMAP.md` under
"What Edit asks of Ouroboros", beside the editor-console item, not started.

Checked: `rename` is declared in picolibc's `stdio.h` and defined nowhere
in `libc.a` or `libc/src`; `FSOP_MV`'s doc comment
(`syscall-abi/src/lib.rs`) still says the destination must not exist,
while the roadmap records the 2026-09-02 fix that made `mv` replace an
existing ordinary file. The wrapper is the same shape as Proem's `unlink`
(one `np_request`), so the two will land together. The doc comment gets
corrected in the same change.

Done 2026-10-05, merged as #215 (`e0c8e09`). `rename` is in
`libc/src/file.c` over `NP_MV`, and replaces an existing ordinary file at
`newpath` (on FAT32 and exFAT two entry writes ordered so the name always
finds one of the two files, on ext2 one). It answers -1 with EXDEV across
mounts, and EINVAL for a directory moved into itself, which `fsd` now
refuses for every client. Your **Done when** is checked by `make test-crename`
on FAT32 and on ext2: with both present, `rename("/crename.tmp",
"/crename.txt")` gives 0, the target holds the temp's text, and the temp is
gone. `FSOP_MV`'s doc comment no longer says the destination must not
exist.
