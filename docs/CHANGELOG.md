# Ouroboros changelog

Historical record of completed milestones, newest first. For
forward-looking plans, see [`roadmap.md`](roadmap.md); for the
debugging history and lessons behind each decision (what was tried,
what broke, how it was diagnosed), see `CLAUDE.md`; for *how* something
here actually works today, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md).

## Phase 8 — `mv`, and the last real correctness risk in the write-support arc

**The goal:** `mv <src> <dst>`, the last cheap command-level win left in
the "close up the easy write-support commands" arc (phases 4-7) before
moving on to bigger work.

- `Fs::mv` reuses the file/directory's *existing* cluster chain rather
  than reading and rewriting content: locate `src`'s entry, insert a new
  entry for `dst` with the same cluster/size/kind, then free `src`'s old
  entry - same "write the new thing before touching the old one"
  ordering as `write_file`'s overwrite path, so a failure partway
  through (most likely `Error::DirectoryFull` on `dst`'s parent) never
  leaves `src` half-deleted. `dst` must not already exist - `mv` refuses
  rather than overwriting it or moving `src` inside it if `dst` happens
  to be an existing directory, narrower than a real `mv`'s full
  semantics.
- **The one real correctness risk, caught by design rather than by a
  crash:** when a *directory* moves to a *different* parent, its own
  `..` entry has to be patched to point at the new parent (or cluster
  `0`, root's convention, if the new parent is root) - otherwise a
  moved directory's `cd ..` would keep resolving to its *old* parent
  forever, silently. This is the identical cluster-`0`-means-root
  convention from phase 3c's `Fs::find`, reapplied on a write path
  rather than reintroduced as a new bug. Reused `patch_entry_cluster_size`
  (phase 6) to make the fix, rather than writing a new low-level sector
  patcher for it.
- One new syscall, `fs_mv` (14, `(src ptr, src len, dst ptr, dst len)`),
  and a new `mv <src> <dst>` shell builtin - the cheapest of the write
  commands to add on the shell side, since it's a single syscall call
  with no buffer/content handling at all, unlike `cp`.
- **Confirmed working end to end, with the `..`-fixup specifically
  exercised, not just reasoned about:** a same-directory rename; a
  cross-directory move of a plain file; a cross-directory move of a
  *directory* containing its own subdirectory, followed immediately by
  `cd`-ing into the moved directory and back out with `cd ..` - which
  correctly landed at the *new* parent, not the old one; renaming a
  directory in place (same parent) and confirming a doubly-moved nested
  directory's own `..` still resolved correctly afterward; destination-
  already-exists, missing-source, and missing-destination-parent all
  correctly rejected with the source left untouched. Persistence
  confirmed by an actual reboot - all of the above state, including the
  moved/renamed directory tree, was still correct on a fresh mount.
  `make run` (FAT16, no mount) still degrades gracefully. Zero aborts in
  `-d int` cross-checks across every session.
- **Still coarse:** no move-into-an-existing-directory-keeping-basename
  semantics (real `mv`'s most common shortcut); no cycle detection (`mv`
  a directory into its own descendant isn't guarded against); same
  8.3-short-name constraints as `mkdir`/`touch` apply to `dst`'s name.

## Phase 7 — `cp`, pure shell-side plumbing over existing syscalls

**The goal:** `cp <src> <dst>`, the next item phase 6's own changelog
entry and the roadmap parking lot both flagged as newly buildable now
that files can hold real content.

- **No new syscall, no kernel changes at all.** `cmd_cp` reads `src`'s
  entire content into a local stack buffer via the existing
  `fs_read_file` (the same syscall `cat` already uses), then writes
  that buffer to `dst` via the existing `fs_write_file` (phase 6) -
  creating `dst` if it doesn't exist, replacing it if it does, same
  semantics as `write`. Pure shell-side composition of two primitives
  that were already there.
- **Copying a file onto itself is safe by construction, not a special
  case.** The read completes in full, into the shell's own buffer,
  before the write ever starts - there's no window where a partial
  write could clobber a read still in progress.
- **Refuses rather than silently truncates when a source file is too
  large for the shell's read buffer** — a genuinely different judgment
  call than `cat`'s, which prints a truncated prefix with a notice.
  `cat` displaying an incomplete file is merely incomplete; `cp`
  producing an incomplete copy would be a *wrong* copy, so it errors
  outright instead.
- Confirmed working end to end against the real `esp.img`: copy to a
  new destination and to an existing one (overwrite), copy a file onto
  itself (content unchanged), copy a real pre-existing file
  (`INIT.CFG`) to a new name, copy into a subdirectory, and the usual
  error set (missing source, destination is a directory, missing
  destination parent) all correctly handled. `make run` (FAT16, no
  mount) still degrades gracefully. Zero aborts in `-d int`
  cross-checks. No new persistence check needed beyond what phase 6
  already established for `fs_read_file`/`fs_write_file` themselves -
  `cp` adds no new on-disk code path, only a new caller of two already
  reboot-tested ones.
- **Still coarse:** copy size is bounded by the shell's read buffer
  (256 bytes, same as `cat`'s); no recursive directory copy, no `-r`
  flag or any flags at all; no `mv`.

## Phase 6 — `write`, the first way to put real content into a file

**The goal:** close the gap phase 5 left open — every file this kernel
could create was permanently zero bytes, since `touch` only ever
produces empty files. `write_file` is the "actual blocker for `cp`,
output redirection, and anything else that needs a file to hold more
than zero bytes" that phase 5's changelog entry and the roadmap parking
lot both flagged.

- `Fs::write_file` creates a file with exactly the given content, or
  fully replaces (not appends to) an existing file's content. Ordering
  matters: the new cluster chain is allocated and written *before*
  anything about an existing file is touched (freeing its old chain,
  patching its directory entry), so a failure partway through never
  frees or unlinks a file that was already there.
- Two new private helpers: `write_chain` (allocates and writes a fresh
  cluster chain for arbitrary-length data, linking each cluster to the
  next via `write_fat_entry` as it goes) and
  `patch_entry_cluster_size` (rewrites just an existing entry's cluster
  and size fields in place, leaving name/attribute/timestamps alone —
  what makes overwriting different from creating).
- One new syscall, `fs_write_file` (13, `(path ptr, path len, data ptr,
  data len)`), and a new `write <file> <words...>` shell builtin that
  joins the remaining words with spaces (same style as `echo`) and
  writes the result as the file's entire content.
- **A real bug found immediately by testing, not by inspection:**
  `write <file>` with no content words (a legitimate "truncate to
  empty" case, exactly matching `touch`'s own empty-file semantics)
  failed with a generic disk error instead of succeeding. Root cause:
  the syscall's argument-sanity check (`valid_user_range`) rejected any
  zero-length buffer, correct for `fs_list_dir`/`fs_read_file`'s output
  buffers (a zero-length destination is pointless there) but wrong for
  `fs_write_file`'s *input* data, where empty is a real, meaningful
  value. Fixed with a second check, `valid_user_range_allow_empty`,
  used only for the data argument (the path argument still can't be
  empty).
- Confirmed working end to end against the real `esp.img`: create a
  file with content, `cat` shows it exactly; overwrite with shorter
  content, `cat` shows only the new content (no stale trailing bytes
  from the longer original); the empty-write fix specifically
  re-tested and confirmed fixed; writing to an existing directory or a
  missing parent both correctly rejected; a genuine reboot-and-remount
  persistence check (write, reboot, `cat` still shows the content); no
  corruption of pre-existing files; `make run` (FAT16, no mount) still
  degrades gracefully. Zero aborts in `-d int` cross-checks across
  every session.
- **Still coarse:** every write is a full replace, no append and no
  partial/offset writes; content is bounded by whatever the caller can
  fit in one buffer (the shell's `write` command specifically is capped
  by its 128-byte input line, so it can only ever produce a single
  FAT32 cluster's worth of content on this test image — `rm`'s
  multi-cluster cluster-chain-freeing loop, added in phase 5, remains
  logically implemented and reasoned-through but still not exercised by
  an end-to-end test, since nothing in this kernel can yet create a
  file spanning more than one cluster). No `cp`, no output redirection
  yet — those need shell-side plumbing this syscall alone doesn't
  provide (see `roadmap.md`'s parking lot).

## Phase 5 — file lifecycle: `touch`/`rm`

**The goal:** round out phase 4's directory-only write support with the
file equivalent — create and remove files, not just directories.

- `Fs::touch` turned out simpler than `Fs::mkdir`: a real FAT32 empty
  file needs no allocated cluster at all (a directory entry with
  starting cluster `0` and size `0` is a valid empty file per spec), so
  it's just one `insert_dir_entry` call, no `find_free_cluster`/
  `zero_cluster`/`.`/`..` writes. Succeeds as a no-op if the file already
  exists (no RTC to update a modification time with, so "no-op" is the
  honest approximation of real `touch`'s behavior there).
- `Fs::rm` mirrors `Fs::rmdir`'s shape, plus one thing `rmdir` never
  needed: freeing the target's entire cluster chain (a loop over
  `next_cluster`/`write_fat_entry`) before freeing its directory entry -
  a no-op for an empty file, but real for anything with content once a
  future "write file contents" syscall exists.
- Two new syscalls, `fs_touch`/`fs_rm` (11/12, path pointer/length only),
  and matching `touch`/`rm` shell builtins.
- **A latent bug caught by reasoning, not by testing:** `Fs::find`'s
  cluster-`0`-means-root substitution (from phase 3c) applied to *every*
  resolved entry, not just directories - harmless before this milestone
  (nothing but a `..` entry ever had cluster `0`), but `touch` was about
  to make "cluster `0`, and it's a file" common. Fixed by gating the
  substitution on `is_dir` before writing any test that could have hit
  it. See `CLAUDE.md`'s "Phase 5" section for the full story.
- **Still no way to write content into a file** - `touch` only ever
  produces zero-byte files. `cp`/output redirection need a real
  "write file contents" syscall this project doesn't have yet.
- Confirmed working end to end against the real `esp.img`, including the
  same reboot-and-remount persistence check as phase 4, `rm`/`rmdir`
  correctly refusing each other's target (a directory vs. a file), and
  no corruption of pre-existing files.

## A shared syscall-ABI crate

Every "known rough edges"/"next milestone" note since phase 3c had
flagged the same thing: syscall numbers and sentinel values were
hand-duplicated between `kernel/src/syscall.rs`'s dispatch table and
`shell/src/main.rs`'s caller, kept in sync only by convention. Fixed
with a third workspace member, `syscall-abi/` — a plain `#![no_std]`
library crate holding nothing but `pub const` syscall numbers and
sentinel values, no logic. Both `kernel` and `shell` now depend on it
via a path dependency and reference constants directly
(`syscall_abi::FS_MKDIR`, etc.) instead of local, independently-numbered
consts — a future syscall added on one side with the wrong number
literally fails to compile on the other rather than silently
misbehaving at runtime. Confirmed safe against this project's specific
relocation risk (every value is a scalar `u64` const, inlined as an
immediate at the use site under both targets — fundamentally different
from the `core::fmt`/slice-literal-comparison bug, which is specifically
about pointers to literal `.rodata` data). See `CLAUDE.md`'s "A shared
syscall-ABI crate" section for the full story.

Also folded in during the same stretch of work: a real UX bug where
every `fs_*` syscall collapsed "no filesystem is mounted this boot" and
"the filesystem is mounted but this operation failed" into the same
`u64::MAX` sentinel, making every disk command on `make run`'s FAT16
disk look identical to a genuinely broken path. Fixed by splitting the
sentinel (`NO_FS`, `u64::MAX - 1`, distinct from `FS_ERROR`) so the
shell can print an explicit "no filesystem mounted" message instead —
see `CLAUDE.md`'s "Phase 4" section addendum.

## Phase 4 — first filesystem write support: `mkdir`/`rmdir`

**The goal:** cross the write-support line phase 3 deliberately drew, for
the narrowest useful case — creating and removing empty directories — not
the full write surface (`rm`/`touch`/`cp`/`mv`/redirection) at once.

- `virtio_blk::Device` gained `write_sector`, sharing a `submit_request`
  helper with `read_sector` (the only real difference between the two
  requests is the data descriptor's write-flag direction and the
  request-type field).
- `fat32.rs` gained `write_fat_entry` (keeps every FAT copy in sync, not
  just the first — reads only ever needed the first), `find_free_cluster`,
  `zero_cluster`, and `write_raw_entry`, plus `Fs::mkdir`/`Fs::rmdir`
  themselves.
- Two new syscalls, `fs_mkdir`/`fs_rmdir` (9/10, path pointer/length
  only), and matching `mkdir`/`rmdir` shell builtins.
- **Deliberately narrow, not a full write implementation:** no
  directory-extension (a full parent directory makes `mkdir` fail rather
  than growing it), no file creation/deletion, no `cp`/`mv`, and a
  conservative 8.3 short-name character set (ASCII alphanumerics plus
  `_`/`-` only) for names this kernel creates.
- Confirmed working end to end against the real `esp.img`, including a
  genuine reboot-and-remount persistence check (not just a live
  in-memory one), root-removal and already-exists rejection, and
  no corruption of pre-existing files. See `CLAUDE.md`'s "Phase 4"
  section for the full story, including the on-disk write ordering each
  operation follows and why it's ordered that way (claim-before-use for
  `mkdir`'s cluster allocation, check-everything-before-writing-anything
  for `rmdir`).

## Phase 3 — a fully functional shell with disk commands

**The goal:** `ls`, `cat`, `cd`, `pwd` — a shell that can actually browse
and read the filesystem it's running from, not just talk to the console.

**Why this is bigger than phase 2, and can't be shortcut:** every disk
read so far happens during the UEFI boot-services window, before
`exit_boot_services` — a one-shot, one-way door. There is no way to reach
back into UEFI's filesystem protocol once the shell is actually running;
"disk commands" by definition means reading files *after* boot, which
means finally building the runtime storage stack that's been deliberately
deferred twice already (once in the phase-1 shell milestone, again in the
disk-loading milestone) rather than a shortcut on top of what exists. This
phase is really three dependent stages:

### 3a. A real block device driver

- **Transport: virtio-mmio, confirmed and deliberately chosen over
  QEMU's own default.** Addresses confirmed via the same devicetree-dump
  technique as GICv2/the timer (32 slots, `0xa000000`, `0x200` apart).
  Worth recording since it wasn't the obvious path: a plain
  `-drive ...,media=disk` with no `if=`/`-device` actually auto-attaches
  as **virtio-blk-pci**, not virtio-mmio — reaching that at runtime would
  need this kernel's own PCI/ECAM config-space walk (a real subsystem on
  its own, comparable to writing PCI enumeration a second time, since the
  existing `pci.rs` is boot-services-only). The Makefile now attaches the
  drive as virtio-mmio explicitly instead, sidestepping that entirely.
  Modern (non-legacy) register interface, also deliberately chosen and
  verified via direct QEMU-monitor memory peeks before any driver code
  was written — see `CLAUDE.md`'s "Phase 3a" section for the full story,
  including a real bug in the diagnostic process itself (a truncated
  monitor read that briefly looked like "no block device exists at all").
  Parallels' own virtio-mmio behavior is still unconfirmed — same open
  question already on record for virtio-console.
- **Driver scope, as built:** device discovery, feature negotiation (just
  `VIRTIO_F_VERSION_1`), one virtqueue, synchronous polling block reads.
  Write support stayed out of scope, as planned at the time (see phase
  4, above).
- Confirmed working end to end: reads sector 0 back and checks the real
  MBR boot signature, not just "no error returned." See
  `kernel/src/virtio_mmio.rs`/`kernel/src/virtio_blk.rs`.
- **Kernel-resident, not a user-space driver process — a deliberate,
  explicit choice, not an oversight.** `docs/research-minix-boot.md`
  raised a real fork here: writing virtio-blk (and eventually
  virtio-console) as an isolated EL0 driver process would be a concrete
  step toward this project's stated microkernel goal, using the driver as
  the forcing function for dynamic task creation and real IPC. Decided
  against, for now — that would pull the process-model/IPC work (see
  `roadmap.md`'s parking lot) into phase 3's critical path, and phase 3's
  actual goal is disk commands working at all. Revisit once there's more
  than one reason to want driver isolation; virtio itself doesn't
  require it.

### 3b. A filesystem reader

- **Target format: FAT32, as planned** — matches `make image`'s
  `hdiutil -fs FAT32` output, what Parallels ultimately boots from too.
  Real surprise along the way: `make run`'s fast dev-loop disk
  (`fat:rw:esp`, QEMU's `vvfat`) turned out to be **FAT16**, confirmed by
  decoding its BPB by hand before writing any parser code (`BS_FilSysType`
  literally reads `"FAT16   "`) — `make run-image` (Makefile target)
  boots the real `esp.img` instead, since `run`'s disk can never satisfy
  a FAT32 mount. See `CLAUDE.md`'s "Phase 3b" section for the full story.
- **Decided: hand-rolled, not a crate** — turned out to be more than a
  style preference: this reader runs after `exit_boot_services`, where
  the global allocator is no longer valid, and every `no_std` FAT crate
  surveyed assumes an allocator is reachable somewhere in its stack. A
  hard constraint, not just precedent.
- **A second real surprise, since resolved:** this project's own
  `\EFI\OUROBOROS\` directory name didn't fit FAT's 8.3 short-name limit
  (9 characters) — real FAT32 formatters handle that with a long-filename
  (LFN) entry this reader doesn't parse. Resolved at the start of 3c by
  renaming the directory to `\EFI\OUROBORO\` (8 characters) rather than
  implementing LFN parsing — this project controls the name, so once 3c's
  actual need (the shell navigating there) was concrete, renaming was the
  cheaper, more honest fix. `fat32.rs` still has no LFN support in
  general; any *other* 9+ character name is still unreachable.
- Confirmed working end to end: lists `\EFI\BOOT`, reads `BOOTAA64.EFI`
  back (a real multi-cluster file, not a single-block special case) and
  checks its exact size and PE header magic against the real built
  binary. See `kernel/src/fat32.rs`.

### 3c. New syscalls + the commands themselves

- **Syscall shape, as built:** `fs_list_dir`/`fs_read_file` (7/8), each
  taking `(path ptr, path len, buf ptr, buf len)` and writing into the
  caller's buffer directly, rather than the originally-sketched
  `open_dir`/`read_dir`/`open_file`/`read_file`/`close` handle-based
  shape — simpler, and sufficient since nothing here needs a persistent
  open handle across multiple calls. This is also what pushed the
  syscall ABI itself from 1 argument to 4 (`x0`-`x3`) — see `CLAUDE.md`'s
  "Phase 3c" section.
- **All four commands built, all read-only, matching the original
  target:** `ls [path]` (`fs_list_dir`), `cat <file>` (`fs_read_file`),
  `pwd` (shell-local `cwd` state, no syscall), `cd <path>` (shell-local
  state + `fs_list_dir` for validation, no dedicated "exists" syscall).
- **Two real bugs found by testing the actual commands, not by
  inspection** - see `CLAUDE.md`'s "Phase 3c" section for the full
  writeup: a slice/string-vs-literal comparison (`cwd_bytes != b"/"`)
  crashed for the same underlying reason `core::fmt` does (a data
  reference computed for link-time base `0x0`), and a FAT32 `..`-entry
  convention (cluster `0` means "root," not root's real cluster number)
  that wasn't handled hung the *entire system* - masked-IRQ syscall
  context, nothing left to preempt a runaway computation. Both fixed;
  `shell/src/main.rs` also gained path normalization (`normalize_path`)
  as a direct result, collapsing `.`/`..` instead of letting `cwd`
  accumulate them literally.

**Deliberately out of scope at the time** (later picked up as phases
4/5, or still open — see `roadmap.md`): any write support (`mkdir`,
`rm`, `touch`, `cp`, `mv`, output redirection — writing to a real
filesystem is a meaningfully bigger risk than reading, and doesn't block
anything read-only commands need); `stat`/`file`/`find`/`tree`; wiring
`loader.rs` itself to use the new runtime driver instead of
boot-services file I/O (boot-time loading already works and has no
reason to change).

## Phase 2 — real commands

`help`, `echo`, `uptime`, `clear`, with `uptime` backed by a real new
syscall (`get_ticks`) rather than being another echo demo.

## Phase 1.5 — the shell becomes a real process

Moved from a kernel-compiled EL0 blob to a genuine separate program,
loaded from disk at boot and selected by a config file — see
[`processes.md`](processes.md). Unplanned at the start of phase 1, but a
prerequisite for phase 2 meaning anything more than kernel demos.

## Phase 1 — interactive echo shell

Real UART input, a line editor, live character echo with backspace/DEL
handling.

## Phase 0 — boot infrastructure

UEFI entry, console discovery (devicetree/ACPI/PCI), exception vectors,
a real MMU identity map, GICv2 + timer-driven preemption, the syscall
boundary, and two-task preemptive round-robin scheduling. See
[`architecture.md`](architecture.md).
