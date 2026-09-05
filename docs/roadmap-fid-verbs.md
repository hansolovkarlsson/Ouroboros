# The fid verbs reach no export — scoping

**Step 0 of the frontier item, and its whole deliverable: an ordered plan with
a named verification per step, written before any code.** The instruction that
produced it was explicit — the word *"bounded"* describes the symptom, and a
read-through suggested it is not the cause. It is not. The cause is in a
different file from the one the item names, and a perfect `netd` would not fix
the reported symptom.

Grounded in the code as it stands at `4571784` (verified 2026-09-05, by reading
the dispatch chains rather than the comments above them). Where it makes a
call, it states the decision, the rationale, and the alternative rejected.

## The symptom, stated precisely

A C program cannot open a file on a remote mount:

```
$ mount -r 10.0.2.10:564 /mnt/a
$ cat /mnt/a/HELLO.TXT          # works - the shell is path-based
hello from A
$ ./cprog /mnt/a/HELLO.TXT      # fails - open() returns an error
cprog: no such file or directory
```

The path is fine. The message is about a path because `FS_ERROR` is what came
back and "no such file or directory" is what a command makes of it — the trap
`scripts/np9p_server.py`'s own docstring already records costing a real
debugging session. **That misleading message is the first thing to fix, because
every step below is debugged through it.**

## What is actually there

Five fid verbs are defined in `ninep-abi` — `NP_OPEN` (15), `NP_PREAD` (16),
`NP_PWRITE` (17), `NP_FSTAT` (18), `NP_CLUNK` (19) — and exactly one server
implements them.

| | fid verbs | how it fails |
|---|---|---|
| `fsd` | **all five, complete** — `Fid { owner, flags, owner_uid, tree, path }`, `MAX_FIDS = 8`, dead-owner reaping | — |
| `libc/src/file.c` | **calls all five** | hardwired: `MSG_CALL` to `FSD_TASK`, `GRANT` to `FSD_TASK`, no namespace resolution anywhere |
| `netd` export (`build_9p_reply`) | **none of five** | falls to `_ => frame_reply(out, FS_ERROR, &[])` |
| `scripts/np9p_server.py` | **none of five** | four arms served, everything else `FS_ERROR` |
| `ulib` | **none of five** | no `fs_open`/`fs_pread`/`fs_clunk` exists |

`ulib`'s zero is **not a gap to fill.** No Rust caller wants a fid — the shell
and every `/bin` program are path-based, and building the layer first would
repeat the trap the roadmap already names for transitive delegation: *no
consumer exists.* Listed here because its absence looks like an omission and
is not one.

## The root: three gaps, only one of which is where the item points

**Gap A — `libc/src/file.c` never asks where the path lives.** `fsd_request`
sends every verb to `FSD_TASK` and grants to `FSD_TASK`. `open("/mnt/a/F")`
therefore asks `fsd` about `/mnt/a/F`; `/mnt/a` is a *namespace binding*
(`NS_SET`, resolved by `ninep_abi::resolve_ns`), not an `fsd` mount, so `fsd`
answers `NOT_FOUND` and the request never leaves the machine. **This is the
actual root of the reported symptom.** Fixing `netd` alone changes nothing a C
program can observe.

**Gap B — the export gateway does not implement the fid verbs.** Real, and it
blocks the *other* direction (a foreign 9P client opening a fid against this
machine's export). Independent of Gap A.

**Gap C — no foreign observer implements them either.** `np9p_server.py` is the
host-side peer that `make run-image-9p-client` points the guest at. It is the
only independent witness for the client half, so **until it serves fids, Gap A
has nothing to be verified against** — the client would be checked against a
peer that refuses every request regardless.

That ordering is the plan: **C, then observer, then export** — except that the
observer has to come first, because it is the instrument.

## Two decisions that change the plan's shape

> **Both CONFIRMED 2026-09-05**, as recommended: the Rust shim for resolution,
> and `netd` owning the fids. The wording below is the case as it was put; it is
> kept rather than rewritten into a settled statement, because the cost named in
> Decision 1 is one the C arc has not paid yet and should stay visible until it
> has.

### Decision 1 — where namespace resolution lives for C's fd path

`ninep_abi::resolve_ns` is deliberately task-id-neutral and is **the single
source**, shared by `ulib` and `netd`. C cannot call it.

- **Rejected: `fsd` forwards.** `fsd` has no namespace either, and `fsd` calling
  `netd` inverts an existing dependency — `netd` is already an `fsd` client, and
  both are single-threaded `MSG_CALL` servers, so the cycle is a deadlock, not a
  layering complaint.
- **Rejected: reimplement `resolve_ns` in C.** A third copy of the resolver,
  with no compiler and no test laying it beside the other two. There *is*
  precedent for a cross-language copy plus a checker
  (`scripts/check-wire-constants.py`) — but that checks **constants**, which are
  scalars; this is **behaviour**, and a behavioural checker across a language
  boundary is a larger build than the feature.
- **Recommended: a Rust shim compiled into the C link** — a small
  `staticlib` for `aarch64-unknown-none` exposing one function
  (`ns_resolve(path) -> (server, tree, endpoint)`) over `ninep-abi`, so the
  resolver stays a single implementation. The cost is honest and should be
  stated before it is paid: C programs currently link **no Rust at all**
  (clang + LLD, `libc/src/*.c` and picolibc), so this adds a toolchain step and
  a link-order constraint to the C arc. That is the decision to confirm before
  Step 3 starts.

### Decision 2 — who owns a remote fid

`fsd`'s `Fid` carries `owner` (a task **slot**) and `owner_uid`, and the
per-op check is `fids[idx].owner != sender`. If `netd` proxies remote opens
straight through, **every remote fid has `owner == NET_TASK`** — so `fsd`'s
ownership check can no longer separate two remote clients, and `MAX_FIDS = 8`
becomes a budget shared by every local C program *and* every peer in the
cluster. The first of those is a privilege boundary, and it is the same hole
`owner_uid` exists to close locally, arriving from a new direction.

**Recommended: `netd` keeps its own fid table**, mapping *(connection,
client-facing fid)* → *(fsd fid)*, and never lets a remote client name an `fsd`
fid directly. Precedent in the same file: `netd` already keeps per-connection
state this way (`dials: [Option<DialConn>; MAX_DIAL]`). This keeps `fsd`'s
ownership check meaningful (one owner, `netd`) and moves per-client separation
to the layer that actually knows which client is asking.

## The ordered plan

Each step names its check **and** a negative control — the discipline from
[`cluster-keys-postmortem.md`](cluster-keys-postmortem.md): *a step is only
verifiable if the check can fail.* Most of that arc's real findings were checks
that could not.

**Step 1 — make the failure legible. ✅ DONE 2026-09-05.** Both
`_ => FS_ERROR` catch-alls (`netd`'s `build_9p_reply`, `np9p_server.py`'s
`serve_request`) now answer `FS_ERR_NO_SUCH_VERB`, and each server **logs the
verb number** — a status code says *that* a verb is missing and cannot say
*which*. Deliberately **not** `FS_ERR_NOT_SUPPORTED`, whose message reads *"not
supported by this filesystem (mode/owner need ext2)"* and would send a reader
after the filesystem for a verb the *server* never implemented.

Measured on a booted guest (`make run-image-9p`) with
`np9p_client.py … noverb`:

| | `NP_OPEN` (0x10f, no arm) | `NP_STAT` (0x10c, served) |
|---|---|---|
| before | `FS_ERROR`, no log | 27 bytes of stat |
| after | `FS_ERR_NO_SUCH_VERB` + `netd: export: no arm for verb 0x10f` | 27 bytes of stat |

`netd` had **four** such fallthroughs, not one: the `/dev/cons` and `/net` export
arms and the defensive transitive-mount arm answered `FS_ERROR` too, so
`NP_OPEN /net/ip` still said "no such file or directory". All four now answer
alike and name the target that refused (`netd: /net: no arm for verb 0x10f`).

The **host** peer needed a different fix, and it is the one worth remembering:
its refusal was `if NP_BASE <= verb < NP_LIMIT: FS_ERR_READ_ONLY`, and `NP_LIMIT`
is one past `NP_CLUNK` — so all five fid verbs were *inside* it and got
"read-only filesystem", a policy that peer does not have about them. The range
only ever meant "a verb I have heard of", which is a different question, and it
silently absorbed every verb `ninep-abi` added. Replaced with an explicit
mutating-verb set.

That bug survived the first pass because the check only ever ran against the
*guest's* export, while the docstring listing the served verbs — whose own text
says nothing compares it to the dispatch chain — was edited to claim the
opposite of what the code did. So the arms are now covered by
`np9p_server.py --self-test`, in `make test`: one request per verb, checked
against the table beside it. Its negative controls are the bug itself
(restore the range test → four verbs mismatch) and a deleted arm
(→ `NP_STAT` mismatches).

The right-hand column is the control that matters: a probe reporting "not
implemented" for everything, including what *is* implemented, would prove
nothing about either. The left column's "before" row is a real pre-fix build,
booted and probed, not a recollection.

> **Two corrections to this step, found while doing it** — recorded rather than
> smoothed over, since a plan quietly edited to match what happened stops being
> a plan.
>
> **The free slots did not exist.** This step said `MAX-34`…`MAX-38` were free
> below `FS_ERR_MIN`. They are the `ACCT_ERR_*` codes: the band is **full** from
> `MAX-1` to `MAX-38`, and `FS_ERR_MIN` *is* `MAX-38`. Reserving a code
> therefore *moves the floor* — to `MAX-39` — and `libc/include/sys.h`
> **hand-mirrors** that floor, its own comment saying to change both in the same
> commit. A C program compiled against a stale floor reads a newly reserved code
> as an ordinary success value.
>
> **The check could not be run as written.** It named a C `open()` on a remote
> mount. But `libc`'s `open()` returns `-1` for every failure with the status
> discarded, this libc has no `errno`, and `cfile.c` opens a hardcoded local
> path — so nothing C could say would distinguish the codes, and until Step 3 a
> C program cannot reach a remote mount at all. The verb-level check moved to
> the **foreign observer**, which exercises exactly the catch-all under test and
> needs no guest program. Making `open()` surface the code belongs with Step 3,
> where C's routing changes anyway.

**Step 2 — teach the foreign observer.** `np9p_server.py` gains `NP_OPEN`,
`NP_PREAD`, `NP_FSTAT`, `NP_CLUNK` (read-only: `NP_PWRITE` stays refused, the
host export is read-only, and that refusal is now *legible* after Step 1). No
guest code changes. · **Check:** a host-side self-test drives the server's own
dispatch through open→fstat→pread→clunk and compares the bytes against the file
on disk. · **Negative control:** delete one arm, the test fails; the docstring's
hand-maintained verb list is prose that nothing compares to the dispatch chain,
so the test is what makes the list a claim rather than a comment. · **Why
before the client:** it is the instrument, and an instrument built after the
thing it measures gets tuned until the measurement passes
([`blind-instruments-postmortem.md`](blind-instruments-postmortem.md)).

**Step 3 — the C client resolves its namespace.** Decision 1, confirmed first.
`file.c` resolves the path, then addresses `FSD_TASK` or `NET_TASK` accordingly,
granting to whichever it addressed. · **Check:** `make run-image-9p-client`, a
C program `open()`s and reads a file under `/mnt/a` served by the Step-2 server,
and prints bytes that match. · **Negative controls, two:** the same program on a
**local** path still works (no regression on tree 0 — the path every existing C
program takes); and with `/mnt/a` unmounted it fails with a path error rather
than hanging or reaching `fsd`. · This step closes the reported symptom.

**Step 4 — the export learns the fid handle verbs.** `NP_OPEN`, `NP_FSTAT`,
`NP_CLUNK` in `build_9p_reply`, with Decision 2's per-connection table.
`NP_PREAD`/`NP_PWRITE` deliberately **not** yet — the handle lifecycle is worth
proving before the data path rides on it. · **Check:** `np9p_client.py` gains an
open→fstat→clunk sequence against the *guest's* export
(`make run-image-9p`). · **Negative controls, two:** a fid number the client
never opened is refused rather than served; and a fid opened on connection A is
refused on connection B. The second is the whole point of Decision 2, so it is
the one that must be shown failing before the table exists.

**Step 5 — `NP_PREAD`.** The wire→`SAFECOPY` bridge, mirroring
`read_file_chunk`'s existing `GRANT_WRITE` pattern. · **Check:** read a file
**larger than one `NP_REMOTE_CHUNK`** through a fid and byte-compare against the
same file read path-based over the same mount — two independent paths to the
same bytes. · **Negative control:** truncate the expected buffer by one byte;
the comparison must fail. (A same-length compare that passes on a short read is
the failure mode.)

**Step 6 — `NP_PWRITE`.** The mirror bridge; `fsd_write_at` is the proven
precedent for wire-inline → local `GRANT_READ`, already shipped in Phase 2.
· **Check:** the two-VM rig, B writes through a fid onto A's disk, reads it back
and compares. · **Negative control — and this one dictates the rig:** a user
without `w` on the target must be refused. **Use `make run-image-2vm-ext2-*`,
not the FAT32 pair.** FAT32 records no mode, so `fsd` has nothing to enforce and
a permission test there passes before a fix and after it, proving nothing either
time — the caveat `scripts/drive-2vm.py` already carries.

## Deliberately not in scope

- **`ulib` fid helpers** — no Rust consumer, see above.
- **Raising `MAX_FIDS`** — Decision 2 moves the per-client budget to `netd`,
  which makes 8 an `fsd`-local number again. Raise it when something actually
  exhausts it, with the exhaustion as the evidence.
- **`FID_PATH_MAX` (96)** — the export strips the mount prefix before relaying,
  so a remote path arrives *shorter*, not longer. Worth re-checking at Step 4
  rather than pre-emptively widening.
- **Unbounded `cpu` output**, and every other bounded-buffer tail. "Bounded"
  described this item's symptom and was the wrong lead; it should not now
  become the excuse to widen it.
