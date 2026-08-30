# Make it unspellable, not un-grepped (postmortem)

*Design retrospective, a twenty-fifth piece, 2026-08-30 (afternoon). The
attempt at per-user cluster identity that a review rejected — fifteen findings,
five of them independent ways a remote request still reached `fsd` with root
authority. The code is not what is worth recording. What is worth recording is
that the five were not five mistakes: four of them were **one design decision**,
and the decision's flaw was that it made the dangerous thing merely* un-grepped
*rather than* unspellable*.*

## What was being built

The 9P export authenticated a **machine** with a shared cluster key and then
relayed every request to `fsd` under `netd`'s own root identity — so
`check_access` hit its root bypass before any mode was read. An unprivileged
user on node B could `mount -r <A> /mnt/a; cat /mnt/a/etc/shadow` and read every
password hash on node A.

The design answers were, I still think, right:

- **Send the name, not the number.** Two nodes have independent `/etc/passwd`
  files, so uid 1000 need not be the same person on both; NFS's `AUTH_SYS` sends
  the number and silently maps one user onto another whenever the numbering
  differs.
- **Put it inside the MAC.** Free — the MAC was already there — and the claimed
  user becomes untamperable without the key.
- **Resolve it on the far side and refuse a stranger**, rather than mapping
  unknown names to "nobody".

## The spine: a safety property you must remember is not a safety property

`netd` cannot `SET_ID` to the caller (that changes its own task identity, and it
serves many connections from one task), so the identity has to travel as data.
The mechanism I built for that had two properties, and both were the bug:

**Opt-in.** Export-path calls went through `_as` twins (`fsd_call_as`,
`read_file_chunk_as`) that attached the caller's identity; `netd`'s own reads
used the plain names. The stated invariant was *"is every export-path call
proxied? — one grep."*

**A latch.** The identity was sent as a separate `NP_PROXY_ID` message and held
in `fsd` until the next request.

Each property produced its own failures:

| From opt-in | From the latch |
|---|---|
| `NP_WRITE_AT` used `fsd_write_at` — a **fifth** path I had forgotten, so the grep never saw it. Remote `cp`, `>>`, `writeat` all ran as root. | `handle` cleared the latch for **every message from every sender**, so any local `ls` interleaving between the two calls dropped it — and the fallback was `netd`'s root. |
| `NP_CHOWN` needed `a2`, `fsd_call_as` hardcoded `p2 = 0`, so no proxied twin existed and the arm silently used the plain call. Chown to yourself as root, then read legitimately. | Aborts between set and use left the latch armed for `netd`'s *next* call — its own, supposedly-root reads. |
| `Console` and `NetLocal` arms took the identity and returned before consulting it. | |

Plus `NP_RUN` routed *before* the name check entirely, so `cpu` skipped it.

**The grep is the tell.** I wrote the invariant down, believed it, and then
checked it by searching for the four helper *names* I knew about rather than for
the property "calls `fsd`". An invariant that a human enforces by remembering to
look is one that fails silently the first time the code grows a shape the human
did not have in mind.

The rebuild is the opposite shape: identity as a **required parameter** of the
two chokepoints, with an explicit `FirstParty` value for `netd`'s own reads, and
carried **in the request** rather than latched. Then "unproxied" does not
compile. Nobody has to remember anything, and a new verb cannot be added wrong.

## What the evidence said, and what it was worth

The branch had: a green workspace build, clippy clean, 20 host unit tests
passing, zero `R_AARCH64_ABS64`, **a live two-node run showing the exact exploit
refused**, and **a negative control** reproducing the leak with only the source
changes reverted. That is the strongest evidence this project knows how to
produce, and it certified a branch whose headline claim was false.

It was not weak evidence. It was *evidence for the wrong proposition*. It
established "the `cat /mnt/a/etc/shadow` path is fixed", and I read it as "the
file half is fixed". `chown`, `writeat` and `cpu` each defeat the second claim
without touching the first.

## Three about verification, which cost more than the design error

**The health bar had quietly weakened.** Extracting the console harness into a
reusable `Guest` class dropped `SError` from the fault predicate. Every
"0 aborts" reported after that refactor was a weaker claim than the ones before
— including on the feature under review. The number did not change; its meaning
did. `SError` is the asynchronous class a guard-page overrun produces, which
this project has hit five times.

**My verification did not touch the code I changed.** For the crash fix I
altered *write* paths and tested *read* paths, and nearly merged on it. Going
back to exercise writes is what surfaced an intermittent remote `cp` failure —
which then had to be chased honestly rather than filed as a flake, because a
`main`-versus-branch comparison initially looked like a regression I had caused.
It was not: the rewrite is provably a no-op on every input the old expression
did not panic on. Recorded in the roadmap anyway.

**The health bar cannot see a server crash at all.** A userland panic *parks* a
task; it raises no CPU exception, so `-d int` reads 0. The signal is the
supervisor's restart line. A guest that "still boots and prints 0 aborts" can
have had its network server killed and restarted underneath it — which is
exactly what one malformed export frame did.

## The foreign observer, twice in one day

The malformed-frame crash is reachable **only** from outside the guest: it needs
a correctly signed frame with a nonsense length. The thing that could produce
one was `scripts/np9p_client.py`, the host-side Python peer — which exists
because the cluster-auth postmortem's magic-byte bug was caught only by an
independent implementation, and which the rejected branch's flag day had
casually broken. Arguing for keeping a foreign observer alive and then needing
it three hours later is about as direct a demonstration as the argument gets.

The review itself is the other instance, and the more expensive one: a green
suite, a live two-node run and a negative control all agreed, and a foreign
reader disagreed fifteen times.

## Lessons

1. **Make the wrong thing unspellable, not un-grepped.** An opt-in safety
   wrapper is a convention; a required parameter is a rule. If the invariant's
   enforcement mechanism is "someone remembers to check", it will fail at the
   first shape you did not anticipate.
2. **Prefer carrying state with the request over latching it beside the
   request.** A latch has a lifetime, and every other actor in the system is a
   way for that lifetime to end early.
3. **Read your evidence for what it establishes, not for what you hoped.** A
   passing exploit-refusal test proves one path is closed. Enumerate the paths
   before generalizing from it.
4. **Check that your verification touches the code you changed.** Obvious, and I
   nearly shipped a write-path fix verified entirely by reads.
5. **A refactor can weaken a signal without changing its output.** The health
   bar still printed a number. Re-verify what a signal *means* after touching
   the code that computes it — and prefer a signal that says "unavailable"
   rather than "0" when it does not know.
6. **Know what your health signal cannot see.** `-d int` cannot see a userland
   panic, because a parked task is not a fault.
7. **On a security boundary, review is part of the work, not a formality after
   it.** Three of today's four merged security changes were improved by a
   reviewer; one was rejected outright. The cost of the rejected branch was a
   day; the cost of merging it would have been a hole with a demo that looked
   like proof.

## What this day did not finish

The rebuild. The design is settled — identity as a required parameter of
`fsd_call3`/`read_file_chunk` with an explicit `FirstParty`, carried in the NP
request (offset 40 / `a3` is free) rather than latched, which covers `NP_RUN` at
the same time. PR #42 is kept open, marked *do not merge*, with the five
root-authority holes tabulated, so none of it needs re-deriving.
