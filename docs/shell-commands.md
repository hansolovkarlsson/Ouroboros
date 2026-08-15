# Ouroboros shell commands

Reference for the builtin commands in `shell/` — Ouroboros's default
userland shell, loaded from disk at boot (see
[`processes.md`](processes.md)). This document describes *what the
commands do and how to use them*; for the syscalls backing them see
[`architecture.md`](architecture.md)'s syscall table, and for the
debugging history behind specific design choices (why `cd`'s path
resolution works the way it does, why some errors read "no filesystem
mounted", etc.) see `CLAUDE.md`'s phase write-ups. Since the shell is
just a loaded program like any other (`docs/processes.md`), this
document describes the *default* one — a replacement program is free to
implement a completely different command set.

## General behavior

- **Line editing.** Backspace and DEL both erase the previous character
  (standard destructive-backspace: `\b`, space, `\b`). Enter (`\r` or
  `\n`) submits the line. A full 128-byte input buffer silently drops
  further typed characters rather than erroring.
- **Tokenization: whitespace-split, no quoting.** `echo "a b"` sees the
  literal words `"a` and `b"`, not one quoted argument — there is no
  quote-stripping. Every command except `echo`/`write` only ever looks
  at its *first one or two* words after the command name (`cp`/`mv`
  take exactly two paths; every other command just one); any further
  words are ignored (`mkdir a b c` creates `a` and says nothing about
  `b`/`c`).
- **No pipes, redirection, globbing, `;`/`&&` chaining, environment
  variables, command history, or tab completion.** One command per line,
  typed in full, every time.
- **Path resolution** (`ls`, `cat`, `cd`, `mkdir`, `rmdir`, `touch`,
  `rm`): a leading `/` is an absolute path; anything else is resolved
  against the current working directory. `.` and `..` are collapsed
  client-side before the path ever reaches the kernel (`..` past root
  simply stays at root, same as a real shell) — see `resolve_path`/
  `normalize_path` in `shell/src/main.rs`. An empty argument to a
  path-taking command means "the current directory" (so `ls` with no
  argument lists `pwd`'s output; `cd` with no argument re-resolves to
  the same directory rather than jumping to root, unlike a real shell's
  bare `cd`).
- **Name matching is case-insensitive**, same as FAT itself (`ls`/`cat`/
  `cd` on `boot`, `Boot`, or `BOOT` all find the same entry).
- **No mounted filesystem this boot** (e.g. `make run`'s fast dev-loop
  disk, which is FAT16 — see `fat32.rs`) makes every disk command print
  one shared message rather than a command-specific error, since no path
  could ever resolve in that state. Use `make run-image` (or a real
  Parallels/`esp.img` boot) for disk commands to do anything.
- **Only 8.3 short filenames.** No long-filename (LFN) support in the
  underlying FAT32 reader — `mkdir`/`touch` additionally only accept
  ASCII alphanumerics, `_`, and `-` in names they create (one optional
  `.` splitting an up-to-8-character base from an up-to-3-character
  extension). Existing on-disk names outside that set (created by a real
  formatter) can't be created by this shell, and LFN entries aren't
  parsed at all — they won't show up in `ls`.

## Commands

| Command | Syntax | Description | Notes |
|---|---|---|---|
| `help` | `help` | Lists every builtin command name. | Static text, no syscall. |
| `echo` | `echo [words...]` | Prints all words after `echo`, space-separated. | The one command that uses more than its first argument. |
| `uptime` | `uptime` | Prints the preemption tick count since boot. | Backed by `get_ticks` — real kernel state, not a demo. |
| `clear` | `clear` | Clears the screen (ANSI `\x1b[2J\x1b[H`). | A raw escape sequence the shell sends itself, not a syscall — the console has no notion of a screen. |
| `pwd` | `pwd` | Prints the current working directory. | Shell-local state only, no syscall. |
| `ls` | `ls [path]` | Lists a directory's entries, `name` for files and `name/` for subdirectories. Defaults to the current directory. | Truncates rather than erroring if the listing doesn't fit a 256-byte buffer. |
| `cat` | `cat <file>` | Prints a file's contents. | Truncates at 256 bytes with a notice if the file is larger; a file argument is required. |
| `cd` | `cd [path]` | Changes the current working directory. | Validates the target exists and is a directory first (via a listing call — there's no dedicated "does this exist" syscall). |
| `mkdir` | `mkdir <dir>` | Creates an empty subdirectory. | Fails if the name already exists, is invalid, the parent is missing, or the disk is full — all four collapse to one error message. No directory-extension: fails rather than growing a full parent directory. |
| `rmdir` | `rmdir <dir>` | Removes an *empty* subdirectory. | Fails if it doesn't exist, isn't empty, or is root. |
| `touch` | `touch <file>` | Creates an empty (zero-byte) file, or succeeds silently if one already exists there. | There's no RTC on this kernel, so unlike real `touch`, an existing file's "timestamp" isn't updated — nothing happens, successfully. Fails if the target is a directory. |
| `rm` | `rm <file>` | Removes a file. | Fails if it doesn't exist or is a directory — use `rmdir` for those. |
| `write` | `write <file> [words...]` | Joins every word after the filename with a single space (same style as `echo`) and writes the result as the file's *entire* contents, replacing whatever was there. Creates the file if it doesn't exist. | `write <file>` with no words truncates the file to empty (a real, valid case, not an error). Fails if the target is an existing directory or the parent is missing. |
| `cp` | `cp <src> <dst>` | Copies `src`'s contents to `dst`, creating `dst` if it doesn't exist or replacing it if it does. | Reads `src` fully into a 256-byte buffer before writing anything to `dst`, so copying a file onto itself is safe. Refuses (rather than truncating) if `src` is larger than that buffer. No recursive directory copy. |
| `mv` | `mv <src> <dst>` | Renames or moves a file or directory to `dst`. | `dst` must not already exist — fails rather than overwriting it or moving `src` inside it if `dst` happens to be a directory. Moving a directory to a different parent correctly updates its own `..` entry, so `cd ..` inside it still resolves to the *new* parent afterward. |

Any other input prints `unknown command: <word>`. A blank line (just
Enter, or only whitespace) does nothing.

## Known limitations

- **`write`/`cp` always fully replace a file's contents — no append, no
  partial/offset writes, and no output redirection (`>`/`>>`) yet.**
  Redirection would need shell-level parsing this kernel doesn't have
  (splitting `cmd > file` into a command and a target) — see
  `docs/roadmap.md`'s parking lot. `write` content is also bounded by
  the 128-byte input line, so nothing typed at the shell can ever
  exceed one FAT32 cluster's worth of content; `cp` is bounded by its
  own 256-byte read buffer instead.
- **`mv` has no move-into-an-existing-directory-keeping-basename
  shortcut** (real `mv`'s most common everyday case beyond a plain
  rename) and no cycle detection (moving a directory into its own
  descendant isn't guarded against).
- **Every filesystem error beyond "no mounted filesystem" collapses to
  one generic message per command** (e.g. `mkdir`'s "already exists, bad
  name, parent missing, or disk full" covers four genuinely different
  failures) — the underlying syscalls report only two sentinel values,
  not a specific reason. See `architecture.md`'s syscall table.
- **`cd ..` from `/` stays at `/`** rather than erroring — a minor,
  deliberate rough edge, not a bug.
- **Disk-command pointer/length arguments are trusted by the kernel, not
  validated** against this program's actual mapped region — fine while
  this is the only, trusted userland program (see `processes.md`'s
  "known rough edges").
