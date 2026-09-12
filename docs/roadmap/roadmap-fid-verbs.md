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
[`cluster-keys-postmortem.md`](../postmortems/cluster-keys-postmortem.md): *a step is only
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

**Step 2 — teach the foreign observer. ✅ DONE 2026-09-05.**
`np9p_server.py` serves `NP_OPEN`, `NP_PREAD`, `NP_FSTAT` and `NP_CLUNK`;
`NP_PWRITE`, and an `NP_OPEN` asking for write/create/truncate, are refused
`FS_ERR_READ_ONLY` — a policy refusal, now distinguishable from the
"no arm" answer Step 1 reserved. No guest code changed, so the peer is
deliberately **ahead of** the guest: the client in Step 3 can be built against
something that already answers.

· **Check:** `--self-test`, in `make test` — 25 verb/parameter cases against the
table beside the dispatch, then a real round trip (open → fstat → pread in *two*
chunks → clunk → the clunked fid must not still read), byte-compared against the
file. · **Negative controls, all four run and all four caught:** `NP_PREAD`
ignoring its offset; `NP_CLUNK` not freeing the fid; `NP_OPEN` ignoring the
write flags; `NP_FSTAT` reporting a size one byte short.

> **The trap this step found, and it is aimed straight at Step 4:**
> **`NP_OPEN` does not use the parameter layout every other path verb uses.**
> Its `a0` is the `OPEN_*` **flags** and `a1` is the path length — the reverse
> of every other path-carrying verb. `netd`'s `build_9p_reply` decodes `p0` as
> the path length *generically*, before the verb match, so an `NP_OPEN` arriving
> there today would resolve a 1–3 byte path from the flag word and land
> somewhere plausible rather than failing.
>
> This is not a deduction from reading the ABI — the first version of the
> self-test sent a generic frame, so a 10-character path arrived as flags
> `10 = OPEN_WRITE|OPEN_TRUNC` and was refused read-only. The harness written to
> check the trap reproduced it.
>
> And the other four fid verbs carry **no path at all** (`a0` is the fid), so
> re-resolving a path per operation is not merely wasteful — there is nothing to
> resolve. **The fid must remember what it was opened on**, which is an argument
> for Decision 2 independent of the ownership one: only `netd` holds the
> resolution, so only `netd` can own the handle.

> **A follow-up this step declined to fix, on purpose:** `fsd` answers a bare
> `FS_ERROR` for a bad or not-yours fid — the same over-generic sentinel Step 1
> just stopped using for verbs, one layer down. The Python peer **mirrors** it
> rather than improving on it: an observer that answers better than the server
> it observes hides exactly the divergence it exists to find. Worth its own
> small change, not a silent divergence here.

**Step 3a — the build gate. ✅ DONE 2026-09-05.** Decision 1 said a Rust
`staticlib` shim, and named its cost: C programs link no Rust at all. So the
crate was written as a **gate before any logic** — a trivial `x + 1`, linked and
relocation-checked — on the precedent that a one-build gate proved `alloc` could
not be PIE-linked before a week went into it
([`capability-and-hardening-postmortem.md`](../postmortems/capability-and-hardening-postmortem.md)).

**The trivial gate passed and was worthless.** Making it *representative* — the
shim actually calling `ninep_abi::resolve_ns` — **failed the link**:
`rust-lld: error: relocation R_AARCH64_ABS64 cannot be used against local
symbol`, out of the **prebuilt `core`** bundled into the staticlib. The same
wall that makes `alloc`'s collections unlinkable here, one crate down. Bisected
to `resolve_ns` specifically: a version with the call removed links, the version
with it does not.

Resolved by `--gc-sections`: the offending `.rodata` is **unreferenced**, so
collecting it removes the relocation rather than hiding it. Verified 0 ABS64 /
7 RELATIVE afterwards, with the entry point still at `0x0` (the linker script
`KEEP`s `.text.start`). LLD's `-O2` reintroduces the failure, so it is not used.
The flag is now load-bearing, and its comment says so.

**Then it was booted, because a link is not a run.** `/bin/NSDEMO`, on a guest,
with real bindings in place:

```
/EFI/ORBS/INIT.CFG -> fsd tree 0, path /EFI/ORBS/INIT.CFG
/mnt/a/HELLO.TXT   -> REMOTE 10.0.2.2:5641, path /HELLO.TXT
/dev/cons          -> console
/net/ip            -> netd /net, path /ip
```

0 fault lines in QEMU's own trace. The first run of this demo resolved
*everything* to `fsd tree 0` — correct, because nothing was bound, and therefore
proof of nothing; the bindings had to be made before the output meant anything.
The remote line is the one that matters: it is exactly the knowledge a C program
could not previously have, and it is why `open("/mnt/a/F")` went to `fsd`.

**A blind spot found on the way:** `check-relocs` scanned only
`target/aarch64-unknown-none/release`, so it checked 56 Rust binaries and
**zero C ones** while reporting the contract for "every userland binary" — and
the C link is precisely where the new ABS64 risk lives. Widened to `build/*.elf`
(56 → 61). Both controls run: relinking without `--gc-sections` fails at link
time, and a forged ABS64 in a C binary is caught.

**Step 3b — rewire `file.c`. ✅ DONE 2026-09-05.** Decision 1, confirmed first.
`file.c` resolves through `ouro_ns_resolve`, then addresses `FSD_TASK` or
`NET_TASK`, **granting to whichever it addressed** — granting to `fsd` and
sending to `netd` hands the wrong task the buffer.

Measured on a booted guest against the Step-2 host peer, 0 fault lines:

```
$ cremote                                  # before the mount
/EFI/ORBS/INIT.CFG: fd=3 size=16 first=\EFI\ORBS\SH.BIN
/mnt/a/HELLO.TXT: open failed: no such file or directory     -> exit 1
$ mount -r 10.0.2.2:5641 /mnt/a
$ cremote
/EFI/ORBS/INIT.CFG: fd=3 size=16 first=\EFI\ORBS\SH.BIN
/mnt/a/HELLO.TXT: fd=3 size=1960 first=line 000: hello from the host 9P server over TCP
```

**The reported symptom is closed.** Both negative controls are inside the test
program rather than beside it: the local path is read **first** (every existing
C program takes that route, so a change fixing the remote case by breaking the
local one would otherwise look like a pass), and the unmounted run is the same
binary failing cleanly. `cfile` — the existing `O_CREAT|O_TRUNC` user — was run
in the same session and is unaffected.

> **The fd is no longer the fid**, and that identity could not have survived.
> "A POSIX fd IS a 9P fid" was exact while `fsd` was the only server able to
> issue one. With two, a remote fid 3 and a local fid 3 are different handles
> wearing the same number, and both indexed the same `g_files` slot. The fd is
> now a slot this library chooses, with the server's fid stored beside the
> target it belongs to — Decision 2's problem arriving from the client side.

> **A real `fsd` bug this step surfaced, and fixed:** `NP_OPEN` never checked
> that the file exists unless `OPEN_CREATE`/`OPEN_TRUNC` was set. A read-only
> open of an absent path allocated a fid and returned it, so the caller learned
> the truth at its first `NP_PREAD`/`NP_FSTAT` — which is how the pre-mount run
> above first reported *"fstat failed"* for a file that never opened. It broke
> three things at once: POSIX (`open(O_RDONLY)` on a missing file must fail),
> `ninep-abi`'s own claim that permission is checked "here, once, per the
> flags" (untrue of a file that does not exist), and agreement with
> `np9p_server.py`, which answers `FS_ERR_NOT_FOUND`. **A foreign observer
> found a server bug by disagreeing with it**, which is the entire argument for
> building the observer first.

> **Review found a silent zero-byte remote write, and it was the code this step
> shipped untested.** `write()` on a remote fd granted the buffer to `netd` and
> sent `NP_PWRITE` with no payload — but **no grant crosses a machine**: the
> relay forwards the NP message verbatim and never looks at one. The far side
> would have received a bare 48-byte header while `write()` advanced the offset
> and returned `count`. The comment claimed `netd` bridged the grant, which
> confused the *export* side (`fsd_write_at`, inbound, which does bridge) with
> the outbound relay, which does not — `true-when-written` again, written the
> same day. Now **refused**: the inline wire shape for `NP_PWRITE` is not
> defined until step 6, so there is nothing to agree with and nothing to test
> against, and a refusal is checkable today where a second untested
> implementation would not be.
>
> **The step's other fix was half a fix.** Making `open(O_RDONLY)` fail on a
> missing file left the sibling branch alone, so `open(O_WRONLY|O_TRUNC)`
> without `O_CREAT` still *created* the file — the same POSIX violation, one
> branch over. Both are now asserted by `cremote`, which checks that each
> refusal actually happens: a fix that makes something fail correctly needs a
> check that the failure occurs, or it is indistinguishable from the bug still
> being there.

> **Two costs paid here, stated:** every C program now links the Rust shim, so
> `--gc-sections` is required for the whole C arc rather than one target; and
> `open()` still returns `-1` for everything, with `ouro_last_fs_status()`
> beside it as the way to ask why. That is deliberately not `errno` — one
> value, no thread story, and no claim to be more.

> **BLOCKER FOUND 2026-09-05, before writing any of step 4: there was no
> connection to key a fid table on.** `tcp_get` opens a **fresh TCP connection
> per remote request**, with a new source port each time ("One connection", and
> `next_src_port` exists so back-to-back round trips never reuse a 4-tuple a
> peer still holds in `TIME_WAIT`). The export therefore saw one request per
> connection, and a per-connection fid table would have lost every fid the
> instant it was created.
>
> Decision 2's *substance* survived — `netd` owns remote fids, and a remote
> client never names an `fsd` fid directly. What was gone was the **key**, and
> with it the free answer to a question the connection had been answering
> implicitly: **when is a remote fid reclaimed?**

## Decision 3 — the export connection becomes a session (2026-09-05)

**Confirmed: make the connection persistent for a fid's lifetime.** Four
options were weighed against three criteria, in this order — **stable, safe,
and leaving room for future features** — and on those the smallest change was
the worst one.

**That ordering is now the project's standing rule, and it was adopted because
of this decision.** The first recommendation here was the cheapest option, and
it was wrong; the criteria were restated (2026-09-05) as a correction, against
a background of too much time spent re-fixing earlier fixes. A cheap repair
has the same defect rate as the code it repairs — see
[`repairing-the-repairs-postmortem.md`](../postmortems/repairing-the-repairs-postmortem.md) —
so cheapest-now is not cheapest-in-total, and it is not the default answer.

**Why not "translate fid ops to path ops" (the smallest change).** `libc` could
have remembered the resolved path for a remote fd and sent `NP_READ_AT` /
`NP_WRITE_AT` / `NP_STAT`, all of which the export already implements. It needs
no server state at all, and it would have closed remote write almost for free.
It was rejected because path-translation is not merely "authorize per op
instead of once" — it **structurally cannot express** things a handle can, and
never will:

- **A path re-resolved per operation is a TOCTOU.** Between a read at offset 0
  and one at offset 512, the far side can rename or replace that path, and the
  client splices two different files together with no error anywhere — the
  well-formed wrong answer this project keeps writing postmortems about. A fid
  names the *file*; a path names a *name*.
- **Unlink-then-read is impossible.** POSIX lets an open fd keep reading a
  deleted file. Path-based, that cannot be done at all.
- **File locking, `O_APPEND` atomicity, and directory streams** all need
  per-handle server state, so none of them could ever exist remotely.

**Why not a table keyed on the authenticated identity.** It keeps fid semantics
but replaces a structural lifetime with a *policy* one — LRU or a timeout — so
a live handle can vanish under its holder. That risk is permanent, and it is
the hardest part of a design that need not have it.

**What a session buys beyond fids.** Close-clunks-everything is a lifetime with
no timers, no eviction and no knobs. It is also the only one of the four with a
natural home for **session-scoped authentication**: every remote request is
individually signed today, and a session could authenticate once instead —
a reduction in per-request crypto that the other options have nowhere to put.
**Deliberately out of scope for this arc**: changing the auth model in the same
change that changes the transport is two risky things at once. Per-request
signing stays; the door is simply no longer closed.

**The cost, stated honestly.** `MAX_CONNS` is **4**, so sessions are a real
budget and need a per-peer allocation or one peer starves the others. And a
session does **not** remove every lifetime question — it removes the *policy*
one. A peer that crashes without a FIN leaves a half-open connection, so an
idle timeout is still required; it is just TCP-shaped and standard rather than
a fid-eviction rule invented here.

> **GATE RESULT 2026-09-05: viable, but NOT as an unconditional change — and
> the prototype was reverted rather than kept.** The gate was built (the export
> holds the connection after a reply, resetting only once everything is acked so
> retransmission out of `c.prefix` still works, plus a 30s idle reap for a peer
> that dies without a FIN) and it built clean. Then the blocking fact turned up
> on the *client* side:
>
> **Both existing clients use the server's FIN as the end-of-reply marker.**
> `np9p_client.py`'s `recv_reply` reads to EOF; `netd`'s own `tcp_get` says so
> in as many words — *"Receive the response until the peer's FIN or a deadline
> (~1s)"*. An export that stops sending FIN therefore hangs the Python peer
> outright and stalls every guest-to-guest request into a one-second timeout.
> That is a **flag day on a live protocol**, which fails the stability test
> before any of the fid work begins.
>
> The frame already carries its own length (`[u32 len][body]`), so EOF was
> never *necessary* — only convenient. Two things are therefore prerequisites,
> in this order, and neither is optional:
>
> 1. **Make every client length-aware** — read exactly `4 + len` and stop.
>    Verifiable on its own against the current FIN-closing export, so it can
>    land before anything else moves. **✅ DONE 2026-09-05 for the FRAMED
>    path** — `np9p_client.py`'s `recv_reply` and `netd`'s `tcp_get`. Not
>    *"unchanged"*, which is how this was first written and is too strong: the
>    client now closes the connection itself once the framed reply is complete,
>    instead of waiting for the export's FIN. Same bytes, different party
>    tearing down — and getting that wrong strands the exporter's slot for
>    seconds out of a pool of four.
>
>    **"Every client" was an overclaim, and the remainder is a WIRE gap rather
>    than a missed edit.** Both peers keep a second read path that still ends on
>    the peer's FIN — `np9p_client.py`'s `run_op` and `netd`'s `tcp_run`, the
>    remote-execution (`cpu`) stream. Those cannot be made length-aware:
>    **`NP_RUN`'s reply carries no length prefix at all.** It is a raw output
>    stream, and EOF is genuinely its terminator.
>
>    So a session that stops sending FIN between requests would hang `cpu` —
>    `run_op` until its 10s socket timeout, `tcp_run` into its own deadline on
>    every remote run — which is precisely the flag day this prerequisite
>    exists to avoid, surviving in the one verb nobody looked at.
>
>    **Step 2's wire signal must therefore cover the run stream too**, or
>    `NP_RUN` must be excluded from sessions explicitly. Either is a decision;
>    the one thing that is not available is assuming prerequisite 1 finished
>    the job.
> 2. **Make the session OPT-IN**, so an old client and a new export can
>    coexist. Without a signal, a length-aware new client and a FIN-waiting old
>    one cannot both be right about the same server. This needs a wire
>    signal, and inventing one unilaterally is exactly what "do it right the
>    first time" rules out — **the mechanism is a decision, not an
>    implementation detail** (a new auth magic beside `AUTHNP03`, a flag in the
>    request header, or a separate listen port each have different costs across
>    the three implementations that spell this frame).
>
> **The gate did its job.** It cost one afternoon's prototype and it stopped a
> change that would have broken every existing peer, which is precisely the
> failure mode step 3a's gate caught for the toolchain.

> **DECIDED 2026-09-07: the signal is a verb, `NP_SESSION` (`NP_BASE + 0x21`),
> sent first on a fresh connection.** Scored against the three: a new auth
> magic fails *stable* (an old export refuses it as a KEY failure, so a new
> client cannot tell "old server" from "bad key"); a flag bit has no spare
> word and every relay would have to mask it; a second port leaks the mode
> into `mount -r host:port` and doubles every rig line. A verb is the
> mechanism `FS_ERR_NO_SUCH_VERB` (step 1) was built for: an old export answers
> it with the one status that cannot be mistaken for anything else, nothing is
> masked, `check-wire-constants` pins the verbs (since the review of #123: it
> pinned none before, and this sentence was first written as if it did), and the peer self-test
> already drives one request per verb. It is also the natural home for
> session-scoped authentication later, with the magic untouched until the auth
> model itself changes. **`NP_RUN` is excluded from sessions by rule**: its
> reply is a raw stream whose only terminator is the FIN, so on a session it
> falls to the fs dispatch and is refused as "no arm", and `cpu` keeps its own
> connection by construction (`tcp_run` opens one). A session refused for
> budget answers a new code, `FS_ERR_BUSY` (`MAX-40`), which moved the floor
> once more; a v0.19.0 node never receives it (it sends no `NP_SESSION`).

**Step 4 — the session gate. ✅ DONE 2026-09-07, all checks and controls
measured on a booted guest, and each shown to fail.** `netd`'s export keeps a
session connection after every reply (the request state resets once the reply
is fully *acked*, since the reply buffer is also the retransmit source), reaps
any server connection silent with nothing in flight for 30 s by RST (a session
whose peer died without a FIN, and the pre-existing HTTP/one-shot leak of a
peer that never finished closing), caps sessions at `MAX_CONNS - 1` so one-shot
traffic always has a slot (`FS_ERR_BUSY` for the next, never an eviction), and
answers a SYN against a full table with RST instead of a silent drop (a drop
now means a hang for as long as a session lasts). `np9p_client.py session-gate`
runs the whole sequence: **11 of 11 PASS** under `scripts/run-guest.sh` (idle
5 s then same connection answers; `NP_RUN` refused framed and in phase three
ways, a valid caller, an unknown user, an unauthorized key; 3 accepted, 4th
`FS_ERR_BUSY`; a one-shot served beside them; none evicted; 5th connection with
the table full: EOF, not a hang; a peer's RST returns its slot at once; a
silent session reaped; the slots back), guest alive at the end, no
`wedged`/`restarted` line, 0 aborts, three runs.

**The first record here said 8 of 8 and was measured through
`drive-qemu.py`, which kills the guest seconds after its last shell step.** The
early checks in those runs were real; the late ones, the reap and the slots
coming back, were measured against a killed process, and a dead guest's silence
is indistinguishable from the export refusing. `run-guest.sh` exists because of
it and asserts the guest is alive before believing a run; the whole trap is in
[`blind-instruments-postmortem.md`](../postmortems/blind-instruments-postmortem.md). **Against `main`'s export** the
gate fails at the first open with `FS_ERR_NO_SUCH_VERB` (exit 1); **with the
cap and the reap mutated out** it fails on the budget (4 accepted), on the
one-shot beside a full table (reset), and on the reap (0 of 4), exit 3.
Recipe in `docs/testing/testing-qemu.md`. **Not in this step, by design:**
`netd` *as a client* does not open sessions yet (the mount-time probe is step
5's, where a fid first needs one); a request still has to arrive in one
segment (unchanged); a client that pipelines a request before the previous
reply is acked has it retransmitted, not lost; and the host peer serves a
session single-threaded, so it must grow a thread per connection before the
guest holds one against it. As written, the step was: prove a persistent
export connection is viable *before* any fid
exists on top of it — the same shape as step 3a, which earned its keep by
failing. A client holds one export connection open across several requests; the
export serves them all on it and closes cleanly. **No fid verbs, no server
handle state.** · **Checks, three, because three separate things could sink
this:** (1) `netd` is not restarted by the supervisor while a session is idle —
it already pumps its event loop during long waits, which is how `cpu` works, so
this is expected to pass and must be *shown*, not assumed; (2) `MAX_CONNS`
sessions can be open at once and the *next* one is refused cleanly rather than
wedging or evicting a live one; (3) an abandoned session (peer killed, no FIN)
is reclaimed by the idle timeout. · **Negative controls:** kill a peer
mid-session and confirm the slot returns; open `MAX_CONNS + 1` and confirm the
refusal is an error the client reports, not a hang.

**Step 5: the export learns `NP_OPEN` / `NP_FSTAT` / `NP_CLUNK`. ✅ DONE
2026-09-12 for the EXPORT half; the client half is deferred, see below.** The
fid table is keyed on the session from step 4: `TcpConn` carries four
`SessionFid` slots (the `fsd` fid behind each, and the remote user who opened
it), the client-facing number is `3 + slot` and means nothing on any other
connection, and the whole table is clunked on `fsd` when the connection goes,
by a `Drop` on `TcpConn` rather than a call at each of the six sites that free
a slot. A one-shot connection is refused every fid verb with
`FS_ERR_NO_SUCH_VERB` ("not on this connection", the mirror of `NP_RUN` on a
session), so a per-request client, which `netd` itself still is, sees exactly
what it saw before. `NP_OPEN` decodes its own parameters (`a0` = flags,
`a1` = path length, step 2's trap) and resolves the path through the export
namespace like every other verb; the console and `/net` refuse it, having no
fid model here or locally. A fid opened by one user is refused `FS_ERR_PERM`
to another on the same session *without* asking `fsd`, because `fsd` drops a
fid whose caller's uid changed (a recycled slot, locally) and on a session that
would let a co-tenant destroy a handle by naming it. **The drop is gone the
same day** (review items 10 and 11, their own PR): `fsd`'s `Fid.owner` is now
the sender's packed task identity rather than its slot, the ownership test for
all four fid verbs including `NP_CLUNK` is `(owner, owner_uid)`, a uid mismatch
is refused and the fid kept, and the reaper frees by identity, so a restarted
`netd`'s fids are reclaimed at the next full table instead of surviving the
boot. `netd`'s own check stays as the layer that knows which client is asking;
with it removed for one run the gate's co-tenant check still passes on `fsd`'s
wall alone (measured). `NP_PREAD`/`NP_PWRITE`
deliberately not yet: the handle lifecycle is worth proving before the data
path rides on it.

· **Check, measured under `run-guest.sh`, guest alive, 0 restarts, 0 aborts:**
`np9p_client.py fid-gate` runs open→fstat→clunk over one session with the
`fstat` record byte-compared against `NP_STAT`'s for the same path (two paths
to the same bytes), then the clunked fid refused. **10 of 10 PASS.** · **The
three negative controls the plan named, all run:** a fid the session never
opened is refused; a fid opened on session A is refused on B while A still
holds it (and A's own `fstat` still served, so the refusal is about the
session and not the fid); closing A holding a fid leaves the number dead on a
fresh session. That third one a per-connection table passes *whether or not*
`fsd` was told, so the gate adds the half that can fail: **12 opens across
three closed sessions against `fsd`'s table** (`ninep_abi::MAX_FIDS`, 8 that
day; the wire checker asserts the gate's product exceeds it on every
`make test`, since a raised table would otherwise leave this check passing
with the clunk deleted), which succeed only if each close clunked. · **Three more, pinning the step's boundaries:** the fifth
open on a session is `FS_ERR_BUSY`; a fid verb on a one-shot connection is
`FS_ERR_NO_SUCH_VERB`; another user on the session is `FS_ERR_PERM` and the
owner keeps the fid; and `NP_PREAD` on a session answers `FS_ERR_NO_SUCH_VERB`
with the session still in phase, a line step 6 must flip. · **Shown failing
three ways:** against `main`'s export before the change, 8 of 10 fail at the
first open with `FS_ERR_NO_SUCH_VERB` (the two boundary pins pass there by
construction); with the `Drop` clunk mutated out, the 12-opens check fails at
the eighth open (7 of 12, one slot already lost to the fid session A was closed
holding earlier in the run); with the uid check mutated out, the co-tenant check fails, and
`fsd` then drops the fid so the owner's next `fstat` fails too, which is the
claim in the paragraph above measured rather than argued. Recipe in
`docs/testing/testing-qemu.md`.

> **The client half is NOT in this step, and the plan's sentence about it was
> under-specified.** "`netd` as a client probing `NP_SESSION` once at
> `mount -r` and remembering the answer per mount" has no check in the plan
> and, read closely, no consumer: `mount -r` is a shell `NS_SET`, `netd` is
> never told a mount happened, and a C program's remote `open()` reaches
> `netd` as one `NETOP_RMOUNT` per verb, each carried by `tcp_get` on a fresh
> connection that closes after the reply. For a fid to survive from that
> `open()` to the `fstat()` after it, `netd` must HOLD a client-side session
> to the far export across requests, which `tcp_get`'s synchronous
> one-connection-per-call model cannot do; the event-loop-driven `DialConn`
> is the shape that can. That is a design decision (where the client session
> lives, when it opens, what happens to it when the peer reaps it after 30 s
> idle, and how it is verified: the two-VM rig, since the host peer serves
> fids on any connection and would pass a client that never held one), not a
> follow-up, and it is recorded here for the next session rather than built on
> a guess. Until it is built, a guest-to-guest C `open()` on a remote mount
> gets `FS_ERR_NO_SUCH_VERB` from a step-5 export exactly as it did from a
> step-4 one.

**Step 6 — `NP_PREAD`.** The wire→`SAFECOPY` bridge, mirroring
`read_file_chunk`'s existing `GRANT_WRITE` pattern. · **Check:** read a file
**larger than one `NP_REMOTE_CHUNK`** through a fid and byte-compare against
the same file read path-based over the same mount — two independent paths to
the same bytes. · **Negative control:** truncate the expected buffer by one
byte; the comparison must fail. (A same-length compare that passes on a short
read is the failure mode.)

> **Fold `fid_verb_reply` into `build_9p_reply` as part of this step, not
> before it (decided 2026-09-12, review of #128 item 9).** Step 5 left the
> fid verbs in their own `fid_verb_reply`, which re-spells `build_9p_reply`'s
> preamble a second way: the runt check, the header decode, and the `EXPORT_NS`
> `resolve_ns` plus the non-`Fsd` `NsTarget` refusal. Removing
> that duplication means threading `fids`/`session` into `build_9p_reply` and
> giving it a per-verb path-word selector (`a0` for the path verbs, `a1` for
> `NP_OPEN`, no path for the fid ops), so `NP_OPEN`/`NP_FSTAT`/`NP_CLUNK`
> become arms of the existing match, and `NP_PREAD` is then one more arm
> beside `NP_READ_AT`, reusing `read_file_chunk` and its chunk cap rather than
> a second copy. That is the payoff, and it is **this step's** consumer: the
> fold touches the dispatch preamble every path verb runs through, which the
> step-5 fid gate does not exercise (it covers `NP_OPEN`/`FSTAT`/`CLUNK`,
> `readdir` and `stat`, not `read`/`write`/`mv`/`chmod`/`write_at`), so it
> should land with the read-path gate this step adds and not as a bare
> refactor ahead of it (the merge-with-consumer lesson,
> [`cluster-phase0-postmortem.md`](../postmortems/cluster-phase0-postmortem.md)).

**Step 7 — `NP_PWRITE`, and the C write path it unblocks.** The mirror bridge;
`fsd_write_at` is the proven precedent for wire-inline → local `GRANT_READ`.
This is also where `libc`'s remote `write()` stops being a refusal: step 3b
refuses it precisely because this wire shape did not exist yet. · **Check:**
the two-VM rig, B writes through a fid onto A's disk, reads it back and
compares; and `cremote`'s `write() to a remote fd` assertion flips from
"must refuse" to "must succeed". · **Negative control — and this one dictates
the rig:** a user without `w` on the target must be refused. **Use
`make run-image-2vm-ext2-*`, not the FAT32 pair.** FAT32 records no mode, so
`fsd` has nothing to enforce and a permission test there passes before a fix
and after it, proving nothing either time — the caveat
`scripts/drive-2vm.py` already carries.

## Deliberately not in scope

- **`ulib` fid helpers** — no Rust consumer, see above.
- **Raising `MAX_FIDS`.** This entry used to say Decision 2 "makes 8 an
  `fsd`-local number again". It does not: every remote fid is also an `fsd`
  fid, owned by `netd`, so `fsd`'s 8 is shared by every local C program and
  every session, and three full sessions (`SESSION_FIDS` = 4 each) exceed it
  (the `SESSION_FIDS` comment in `netd` says so, 2026-09-12). Raise it when
  something actually exhausts it, with the exhaustion as the evidence; the
  step-5 gate depends on 12 > 8 and quotes both numbers, which is a copy that
  can drift and is the review's item 7.
- **Session-scoped authentication** — Decision 3 makes it possible for the
  first time, and it stays out of this arc anyway: changing the auth model in
  the same change that changes the transport is two risky things at once.
- **`fsd`'s bare `FS_ERROR` for a bad or not-yours fid** — the same
  over-generic sentinel step 1 stopped using for verbs, one layer down. Worth
  its own small change; `scripts/np9p_server.py` mirrors it deliberately rather
  than diverging.
- **`FID_PATH_MAX` (96)** — the export strips the mount prefix before relaying,
  so a remote path arrives *shorter*, not longer. Worth re-checking at Step 4
  rather than pre-emptively widening.
- **Unbounded `cpu` output**, and every other bounded-buffer tail. "Bounded"
  described this item's symptom and was the wrong lead; it should not now
  become the excuse to widen it.
