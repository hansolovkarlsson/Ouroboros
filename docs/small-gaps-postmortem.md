# The small-gaps arc — clearing the roadmap parking lot

*A design/bug retrospective (a nineteenth piece, 2026-08-27). The day the
"Open gaps" and "Remaining follow-ups" lists in `roadmap.md` got emptied of
their small items — seven of them, in a row: FAT32 long-filename **write**,
GPT **CRC validation** + backup fallback, `a | b > file` (pipelines compose
with redirection), `grep` **flags** (`-i`/`-v`/`-n`) + a cooperative **`YIELD`**
for prompt filter early-exit, a **builtin at any pipeline position**, **`sort`**
(the filter that can't stream), and **environment export** to child programs.*

For the milestone-by-milestone facts see `CHANGELOG.md`; for the day-by-day
narrative see `journal.md`. This is the retrospective — the threads that ran
through a batch of items each marked "small," and what "small" turned out to
mean.

## The spine: a "small" gap is one whose hard part is a single non-obvious constraint

None of these was a multi-milestone arc. But not one was trivial either, and
the reason is uniform: each had exactly *one* awkward constraint hiding under an
otherwise-turnkey task, and the work was finding it and giving it a clean
answer. The roadmap had honestly flagged most of them ("the one filter with a
real wrinkle"; "would also let a clean delete free the orphaned LFN entries";
"not likely worth changing") — and in every case the flag pointed at the real
constraint. The lesson for the parking lot itself: **the one-line "why it's
deferred" note is usually the whole design problem stated in advance.** Read it
before starting; it's not boilerplate.

Seven items, seven constraints:

## 1. LFN write — the abstraction was already right; the work was the *inverse*

FAT32 could *read* long filenames but only *create* 8.3 ones. The constraint:
a long name needs a generated `NAME~N` short alias, a run of LFN entries
carrying the real name, a checksum binding the run to the alias, and those
entries laid down **physically contiguous**. All of that machinery already
existed on the read side — the checksum function, the character-position table,
the entry decoder — so writing was largely *running the reader backward*.

Two things worth keeping:

- **The bonus the roadmap predicted was real and shared a mechanism.** The note
  said LFN write "would also let a clean delete free the orphaned LFN entries
  `rm` leaves." It did, and `free_entry_with_lfn` matches the target by *exact
  on-disk location* (not by name), so a freshly-inserted `mv` destination in the
  same directory is never confused for the source being freed — the same
  discipline the write path needed for the checksum guard.

- **The foreign observer stayed foreign** (a recurring theme, below): validated
  by macOS's own FAT driver mounting the guest-written image *and* `fsck_msdos`,
  not by our own reader round-tripping. Our reader sharing a bug with our writer
  would confirm the bug; macOS can't.

## 2 & the deepest lesson: firmware robustness can *hide* the bug you're testing

GPT was parsed on trust — no CRC check. Adding the two CRC32s (header and
entry-array) and a backup-GPT fallback was routine. Testing it was not, and
this is the arc's sharpest lesson.

You cannot test GPT-corruption recovery through a normal UEFI boot, because the
firmware is *also* robust:

- Corrupt the **primary** header and boot: `fsd` mounted fine — but the on-disk
  primary was **valid again afterward**. EDK2/OVMF **auto-repairs a corrupt
  primary GPT from the backup during boot**, so by the time `fsd` reads the
  disk there is nothing wrong with the primary. The "test" validated a *healed*
  disk.
- Corrupt **both** copies and boot: the firmware refuses to boot at all
  (`CheckCrc32: Crc check failed` → EFI shell), so the kernel never runs.

Either way the layer under test is invisible behind the firmware's own
correctness. The fix was to stop booting and instead run **the real
`partition.rs` against a mock disk in a host harness** — clean/corrupt-primary/
corrupt-array/corrupt-both/plain-MBR, all five outcomes, with the *actual*
module, no firmware in the loop. Plus a cross-check that the hand-rolled bitwise
CRC-32 produced byte-identical values to `zlib.crc32` and to what the image
builder stored.

**Generalize it:** "test it on real hardware/firmware" is not automatically the
strongest test. When the thing you're testing is a *robustness* path, the
robustness of the layer *around* it can mask the very behavior you want to
observe. A host harness driving the real module is not a lesser test there — it
is the only honest one.

## 3. A missing scheduler primitive, and an honest re-read of the gap note

The roadmap said `head` "relies on the producer's send-timeout when it exits
early rather than actively signalling upstream." Digging in, that was **half
untrue**: a send to an *exited* (zombie) consumer already fails non-transiently
(`task_exists` is false for a zombie), so `pipe_out` returns at once — never on
the timeout. The old code comment overstated it.

The *real* residual cost was a **busy-spin**: while `head` drains its last
buffer, a producer that fills its mailbox gets `MSG_ERR_FULL` and re-sends in a
tight loop until the next tick lets the consumer run — invisible under QEMU's
~37 ms ticks, but a ~1-second stall at a 1-second hardware tick, burning the CPU
the consumer needs. There was no way to hand the CPU over cooperatively: **no
yield primitive existed.** So one was added — `YIELD` (syscall 57), which saves
the caller as still-runnable and switches to another runnable task, *skipping
the idle task unless it's the only one runnable* (the subtlety: a naive yield
lands on the always-runnable idle task and wastes exactly the tick it was trying
to save). `pipe_out` now yields on `MSG_ERR_FULL`.

**Two lessons.** First, re-read a gap note against the code before trusting it —
the symptom (`head`) was named, but the fix belonged one layer down (the shared
`pipe_out`) plus a genuinely missing kernel primitive. Second, `grep` gained
`-i`/`-v`/`-n` in the same change — the *substantive* half of "grep is
substring-only and case-sensitive"; regex stays a separate arc, and saying so is
better than a token attempt.

## 4. The reduction that turned "not worth it" into a small, uniform change

"A pipeline stage other than the first can't be a builtin" was flagged
*"Reasonable; documented, not likely worth changing."* And it's true there is no
*useful* builtin that transforms a stream. But the reason is the whole design:
**a builtin runs in the shell, not as a task, and none read stdin, so a builtin
can only ever be a pipeline's *source*.** That observation is a reduction: a
non-first builtin means "run everything upstream for its side effects, discard
its output, and let the builtin source the rest." `ls | ps | grep runnable`
becomes *(ls drained) then (ps → grep)*.

So the fix was a split + reuse: classify the stages, and for a builtin at index
`k>0`, drain `stages[..k]` (a new no-size-cap discard path) then run
`stages[k..]` through the *existing* builtin-head machinery. The pipeline core
was unified behind a `PipeSink` of `Console | Redirect | Drain` — and the
`a | b > file` redirect from earlier the same day fell out as one of the three
sinks. A "not worth it" became cheap the moment the honest reduction ("a builtin
discards its upstream") was named.

## 5 & 6. `sort` and env-export — reuse the shape you already have

Both of the last two items were solved by *not inventing a new shape*.

- **`sort`** is the one filter that can't stream (it must see every line before
  emitting one), against a no-heap-allocator, fixed-buffer constraint. The
  answer was the 256 KB per-program **heap** already used by the pager: input
  bytes in the front, a line index (start+len) reinterpreted from the 4-aligned
  tail via `align_to_mut::<u32>()` — so the index costs no stack (the spawn stack
  is only 32 KB with a guard page) — and an in-place **heapsort**, keeping
  *working* memory O(1) beyond the input it necessarily holds. The interesting
  constraint (can't stream) had a clean answer (heap + index + heapsort) that
  needed no allocator.

- **Env-export** was built as a near-exact copy of the **argv ABI**: the same
  `[count][len][bytes]…` blob (each entry a `NAME=VALUE` string), the same
  deliver-kernel-side / fetch-by-child shape, the same per-task store cleared on
  death. Three syscalls (`ENV_STAGE`/`GET_ENVC`/`GET_ENV`) that reuse the argv
  *decoders* verbatim. The only genuinely new wrinkle: `SPAWN`'s four argument
  slots were full, so the env has no length arg — `ENV_STAGE` **latches** the
  blob and the next `SPAWN` consumes it. When a new capability is the same
  *shape* as an existing one, copy the shape; the decoders, the lifecycle, and
  the mental model all come for free.

## The env-export bug: the store size is not the read size

Env-export wired up, the kernel debug showed the blob attached
(`env_len=17`, then `28` after two `set`s), the child's `GET_ENVC` returned the
right count — and `printenv` printed nothing. `GET_ENV` was returning "no such
entry." The cause: the read buffer was sized `ENV_MAX` (2048, the *whole-blob*
store size), but `GET_ENV`'s out-pointer is range-checked like every user
pointer, and the syscall boundary caps a user range at `MAX_USER_LEN` = 512. A
2048-byte capacity failed the check, and the syscall bailed *before copying* —
silently, because "range invalid" and "no entry" collapse to the same `NO_ARG`
return.

Fix: read one `NAME=VALUE` entry at a time into a small buffer (an entry is at
most ~153 bytes). **Lesson:** a blob store and a per-item read have *different*
size bounds, and conflating them is invisible when the failure mode is a
silent sentinel. It's now documented on `GET_ENV` itself, because the asymmetry
is non-obvious — the store is big, each read is small. (Found, as ever, by a
`valid_user_range` debug print — the sentinel hid it, the print didn't.)

## What did *not* happen: the PIE str-slice trap stayed retired

Prior arcs kept re-hitting the same wall — slicing a `&str` by a runtime index
pulls in `core::fmt`'s char-boundary panic formatter (`R_AARCH64_ABS64`, an
unlinkable relocation under this crate's PIE model). This arc touched a lot of
new byte-wrangling (`grep`'s flag parse and matcher, `sort`'s line index, the
env serializer/`getenv` split-on-`=`) and hit it **zero times**, because every
one of them worked in `&[u8]` from the first line rather than reaching for a
`&str` slice. The trap didn't retire; the reflex to avoid it did. That's the
shape of a lesson actually learned: not "the bug stopped happening" but "the
code stopped inviting it."

## The tally

Seven parking-lot items closed in a day, each a real (if small) design problem
with a single awkward constraint and a clean answer. The recurring disciplines —
validate against a foreign observer, distrust a robustness test that a robust
layer can mask, re-read the gap note against the code, name the reduction that
makes a hard case cheap, and reuse the ABI shape you already have — are the same
ones the bigger arcs in this project's other eighteen postmortems keep arriving
at. Small work is a good place to practice them, because the constraint is never
buried under scale; it's right there in the one-line note that said the item was
deferred.
