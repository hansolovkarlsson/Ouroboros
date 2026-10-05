# fstat: give each file an identity, or zero what it does not fill

- **From:** Proem
- **To:** Ouroboros
- **Date:** 2026-10-01
- **Kind:** requirement
- **Status:** done
- **Blocks:** Proem giving correct output on Ouroboros for any header that
  uses `#pragma once` or an include guard, which is nearly all of them.

## What is asked

`fstat` should fill `st_dev` and `st_ino` so that two open files have the
same pair exactly when they are the same file, whatever path reached
them. 9P's qid path is the natural `st_ino`, with an `st_dev` fixed for
each file server. If that is not possible yet, `fstat` should at least set
every field it does not fill to zero, so a caller can see that there is no
identity.

`roadmap-c-hosting.md` (branch `docs/c-hosting`, `f060b8d`) covers `errno`
and `stat(path)`, but not this.

## Why

`fstat` in `libc/src/file.c:344` sets only `st_size` (line 354) and
`st_mode` (line 355). Every other field of the caller's `struct stat` is
left as it was, which for a local variable is whatever was on the stack.

Proem knows a file by `st_dev` and `st_ino` from `fstat` on the stream it
opened (`lib/source.c`, `proem_source_open`), and uses that in two places:

- `#pragma once`: an `#include` that finds a marked file does nothing.
- Include guards, since Proem `135a3bd`: an `#include` that finds a file
  already read with an `#ifndef X` guard does nothing while `X` is
  defined. The file is not even read.

With leftover values, two different headers can compare as the same file,
and the second is skipped without any message. The output is then wrong,
missing declarations, with no error to point at it. picolibc's headers are
guarded throughout, and one uses `#pragma once`.

Proem will also guard its own side. It will clear `struct stat` before the
call and take a zero pair as "no identity". That is on Proem's roadmap,
under "File identity without `st_dev` and `st_ino`". With no identity,
Proem must reread every header each time. That is correct but costs the
memory the other note asks for
(`2026-10-01-from-proem-heap-growth.md`). Or Proem could compare paths,
which loses symbolic links and `..`. A real identity avoids both.

## Done when

The reply names the commit. A C program on a booted image opens two
different files, and one file by two paths (through `..` will do), calls
`fstat` on each, and prints `st_dev` and `st_ino`. The two different files
must differ, and the two paths to one file must agree. Or, for the
fallback, the reply shows `fstat` zeroing every field it does not fill.

## Reply

Accepted 2026-10-01 by Ouroboros, for later: on `docs/ROADMAP.md` under "What
Proem asks of Ouroboros", not started.

Checked: `fstat` at `libc/src/file.c:344` sets `st_size` and `st_mode` only
(lines 354 and 355), so the rest of the caller's `struct stat` is left as it
was.

The two halves are different sizes, and will likely land in that order. The
fallback, zeroing every field `fstat` does not fill, is a few lines in
`libc` and meets this note's second **Done when**. A real identity is
larger: the `NP_FSTAT` record is 27 bytes (`STAT_INFO_LEN`, `ninep-abi`) and
carries no inode or qid, so it is a wire change across `ninep-abi`, the two
C headers and the Python peers, which `make test` checks against each other.
Each filesystem then needs an identity: ext2 has inode numbers, FAT32 and
exFAT have none (the directory entry's location is the candidate; the first
cluster is not, since an empty file has none), and `/proc` is synthetic.
`st_dev` has to tell the local server from each remote mount. Proem clearing
`struct stat` before the call and reading a zero pair as "no identity" is
the right guard on its side either way.

Done 2026-10-05 for the fallback, merged as #216 (`8e132c3`). `fstat` now
zeroes the whole `struct stat` and then fills only what the server's record
carries, so `st_dev` and `st_ino` read 0, "no identity", as your second
**Done when** allows. `make test-crename` checks it with `/bin/CFSTAT`
(`libc/cfstat.c`) on FAT32 and ext2: a struct filled with 0xA5 comes back
equal to zero plus size, mode, uid and gid, padding included.

**Distrust anything Proem measured about file identity on Ouroboros before
this.** Building that check found the port's `file.o` compiled against the
hand-rolled `struct stat` (`st_size` at offset 0, `st_mode` at 8), while a
picolibc program's has `st_mode` at 4 and `st_size` at 16. `fstat` wrote the
file's SIZE into `st_dev`/`st_ino` and never set `st_size`. So Proem's
identity was the file size: two headers of the same size compared as one
file, and the second was skipped. Every picolibc port object is now built
against picolibc's headers only.

Also from this change: `st_size` is right; `st_mode` carries the file type
everywhere (`S_ISREG`/`S_ISDIR` were false on FAT32); on ext2 the mode, uid
and gid are the disk's, and on FAT32 and exFAT, which record none, the mode
is 0666 for a file and 0777 for a directory (`fsd` enforces nothing there)
with uid and gid 0.

The real identity, a non-zero `st_dev`/`st_ino` pair, is a wire change and
an identity per filesystem; it is on our roadmap as its own item, "A real
file identity in `fstat`". If Proem wants it sooner than its turn, a new
note naming this one is the way.
