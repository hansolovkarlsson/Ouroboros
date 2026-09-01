# Per-machine keypairs: the arc that replaced a password with an identity

*A design-and-process retrospective — the twenty-sixth — covering 2026-08-31:
the day a shared cluster secret became one Ed25519 keypair per machine, built
from scratch, in eleven reviewed steps.*

Fourteen merged pull requests. A hand-rolled Ed25519 (SHA-512, field arithmetic,
curve points, scalar arithmetic, sign and verify), an on-disk identity format, a
key generator, a verifier, a signer, reply signing, an audit of the whole thing,
and a flag day that deleted the format it replaced. No cryptography crate — the
PIE relocation contract this project runs under (`R_AARCH64_ABS64` is
unloadable) rules out the prebuilt `alloc` collections most of them want, and
that constraint has been load-bearing since the userland heap milestone.

The spine of the day is not the cryptography. It is this:

> **A step is only "verifiable" if the check can fail. Most of the day's real
> findings were checks that could not.**

That sentence sounds like a truism until you count how many times it bit,
in an arc explicitly designed around per-step verification, by someone who had
already written the lesson down twice.

---

## What was actually wrong with the old design

The shared cluster key (v0.10.0–v0.15.0) worked, and its postmortem
([`cluster-auth-postmortem.md`](cluster-auth-postmortem.md)) is still accurate
about why it was the right first cut. Its limit is not a bug:

**Verification capability equals signing capability.** With one symmetric
secret, a peer that can *check* A's requests can *forge* them. That is sound at
two nodes, where "someone who holds the key" and "the other machine" are the
same statement. At three it stops being true, and there is no revocation short
of re-keying everyone.

Asymmetric keys separate those two capabilities, which is the whole point.
`authorized` becomes a list you delete a line from.

---

## The step plan, and why it survived contact

The user's instruction opened the arc and shaped everything after it:

> *"I don't want to repeat the chaos the other day when we did too much before
> we made a review and discovered too many issues. […] There's no pressure on
> time. The only pressure is quality."*

That referenced [`review-and-split-postmortem.md`](review-and-split-postmortem.md)
— the day one branch grew too big to review, and fixing review findings produced
roughly one self-inflicted escalation per round. So the plan doc
([`roadmap-cluster-keys.md`](roadmap-cluster-keys.md)) was written **before any
code**, as eleven steps with a stated verification and a negative control each,
where steps 1–4 touch no existing file and step 4 is a go/no-go gate.

Three things about that plan are worth keeping.

**Steps 1–4 could not break anything.** A new crate with no callers is not a
risk, so the hardest part of the arc (the curve arithmetic) was built where a
mistake cost a test failure rather than a boot loop. The gate at step 4 — *does
this reproduce RFC 8032's published signatures byte-for-byte* — is a stronger
question than "does it verify what it signs", and it is only askable because
Ed25519 is deterministic.

**The plan's own last mile was wrong, and that was fine.** Step 10 was one line:
"flag day, drop the shared key everywhere." Asked what could go wrong, it split
into 10a and 10b, and the split is what found a hole (below). A plan that
survives contact unchanged usually means nobody re-read it.

**The one thing the plan got exactly right was deferring the cost questions.**
Steps 3 and 4 both chose the obviously-correct-but-slow implementation — a
bit-by-bit scalar reduction over ref10's hand-derived constant chain, no
fixed-base table — and wrote down that step 5 would *measure* whether it
mattered. It did measure: ~504 µs to sign under QEMU-TCG, against a network
round trip. Both simplifications stayed, on evidence rather than on taste. The
peak-stack figure (3,392 bytes of 32,768) closed the arc's named headline risk
the same way.

---

## Checks that could not fail

This is the thread. Each of these passed while proving nothing.

### The relocation check had been dead for five steps

`llvm-readelf -r <bin> | grep -c ABS64` returned `0` after every step. It also
returns `0` for a file that does not exist, and — as it turned out —
`llvm-readelf` prints **nothing at all** for these binaries. The number was
never an answer.

The conclusion happened to hold when re-checked with `llvm-readobj`. The
evidence had been worthless anyway. The replacement (`scripts/check-relocs.sh`,
in `make check-relocs`) fails when the total `RELATIVE` count across all
binaries is **zero** — a canary, because a tool that has stopped reporting looks
exactly like a clean result.

### A test that iterated an empty list — twice

Both times a vector-generation script emitted into a splice point that had
moved, so the test looped over nothing and passed. Fixed with
`vectors_are_not_empty` assertions, which is a blunt instrument and the right
one.

### A bound test that could not reach its bound

The field-arithmetic tests included one named for `u64` limb overflow that
performed 256 subtractions. Overflow needs roughly 4,096. Raised to 5,000, at
which point it failed and found a real carry bug.

### Cumulative mutations, silently stacked

Mutation-testing a module without restoring between runs let seven mutations
accumulate. Repairing them by hand restored `add`'s carry and missed `sub`'s —
and **every test still passed**, which is how the missing one was found at all.
Back up the file, restore from the backup, diff against `git` afterwards.

### A truncation probe that passed the offset in the wrong slot

`load_auth`'s "is `authorized` bigger than I can read" probe passed its offset
where `fsd` expects the *tree*, so `fsd` refused every probe as an out-of-range
mount and the warning could never fire. A check that could not fail, inside the
fix for a check that could not fail.

### And the one that mattered most

See below.

---

## The small-order table: wrong in both directions

The whole-arc audit (#59) added a guard refusing small-order Ed25519 public
keys. Against such a key the cofactorless verification equation is satisfiable
with no secret at all, so an `authorized` line carrying one is not a weak
credential — it is a **universal forgery**. Worth guarding.

The guard was a hand-written table of the eight small-order encodings. Three
entries were wrong. It **accepted** three genuinely small-order keys, and
**refused** one ordinary valid point and one string that is not a curve point at
all. Nobody reads 32 bytes of hex and notices that `26e8…` became `13e8…`.

The accompanying test exercised the two entries that happened to be right.

**The fix is not a better table.** `[8]P == identity` *is* the definition of
small order, costs three point doublings, and cannot be transcribed wrongly. It
lives in `ed25519::verify` — the security boundary — rather than in the parser
that happened to think of it.

Then the replacement test was itself vacuous: it asserted `!verify(...)` on a
forged signature that would never have verified anyway, so it **passed with the
guard removed**. Mutation testing caught that. It now constructs a forgery that
genuinely works, in two families, because the first family (`s = 0` with a
small-order `R`) is accidentally caught by a guard placed on the *wrong point* —
the suite stayed green with the check on `R` instead of `A`, which reopens the
forgery. The second family uses a large-order `R` and has no such accident.

Its candidates are now **derived**: one order-8 generator is written down, the
other seven encodings come from adding it to itself, and the subgroup is
asserted to close at eight distinct points. A mistyped generator fails loudly
instead of quietly shrinking the test.

**The lesson generalises past this bug.** Enumerating a mathematical property is
a transcription task; computing it is not. Where a definition is cheap to
evaluate, evaluate it.

---

## The flag day, and why it split

`Auth::enabled()` was literally `key_len > 0`. It gated three things: the
export, inbound authentication, and *outbound signing*. It was written when the
shared key was the only way to authenticate anything, and it kept that meaning
after step 7 gave the cluster a second one.

So a node holding a perfectly good keypair and a full `authorized` file reported
`export CLOSED` and refused its peer — confirmed on two guests **before touching
any code**. Deleting `\CLUSTER.KEY` without moving that gate takes the cluster
dark, and doing both in one PR makes a failure unattributable to either.

**10a moved the gate and deleted nothing.** Its acceptance test is a two-node
cluster running with *no shared key anywhere*: remote mount, `cat`, `cpu`, and
per-user permission enforcement, six signed frames each way and zero MAC'd. That
test is only possible while the deletion has not happened.

**And 10a is where the split paid for itself.** The obvious version of that
refactor — remove the top-level `!auth.enabled()` gate — would have shipped a
hole:

> **HMAC with a zero-length key is a perfectly valid HMAC.**

Removing the gate does not make an unconfigured export *refuse* MAC'd requests.
It makes it accept the ones computed under the empty key, which every attacker
also has. "No key" has to *mean* refuse, explicitly, because the verification it
would otherwise fall out of **succeeds**. Verified by removing that one guard,
rebuilding, and reading a guest's disk from the host holding no secret at all.

**10b was then subtraction**, −412 lines, with the four-configuration matrix as
its regression test. Two places the deletion would have left a guess, both safe
only *because* the MAC had existed as a fallback:

- `np_offset` returned the MAC header size for any frame that was not signed —
  which, with one layout left, means reading arbitrary bytes as a verb. It
  returns `Option` now; "unknown" is a real answer.
- `seal_reply` returned the unsealed length when it could not seal. Harmless
  with a MAC fallback; with none it would send an **unauthenticated body**.

The one thing the deletion had to *add* was a way to **send** a retired frame
(`np9p_client.py --legacy-mac`), because a negative control that cannot produce
the old format proves nothing about the new code refusing it.

---

## The foreign observer, four times

The discipline is older than this arc, and it earned its keep again.

1. **Python's bignums checked the field arithmetic.** No limbs, no carries, no
   reduction chain — so the reference cannot be wrong the same way the code can.
2. **The Python curve reference was itself checked against RFC 8032** before
   anything trusted it. An unchecked second implementation by the same author is
   the same assumptions typed twice, not an independent observer.
3. **A Python peer signed frames the guest verified** at step 7, when no guest
   client existed. A format both ends of which are mine proves only that I am
   consistent.
4. **`rustc` and `clippy` are foreign observers too**, and `cargo doc` turned
   out to be a third: it caught a doc comment that had been absorbed into the
   one below it, so the new security guard's rustdoc opened by explaining
   projective point equality.

---

## Restatements that outlived their code

Prose copies rot invisibly, and this project has
written down before. It recurred, inside the PR auditing that very class:

- `sign.rs` opened with **"Small-order public keys are not rejected … this is
  the check to add if that ever stops being true"** — 230 lines above the check,
  in the commit whose message quoted that sentence as the thing that had stopped
  being true. `CLAUDE.md` carried the same claim.
- The shell printed `remote-mounted (cluster-key auth)` on a run whose wire
  carried **five signed frames and zero MAC'd ones**. The shell does not choose
  the format — netd does, per frame — so it was restating a fact it did not own.
- A comment on `load_auth`'s identity read claimed to prevent the exact failure
  the code around it was causing (a shared retry budget that could leave the
  later reads with *zero* attempts).
- `reduce128` documented itself as running two carry passes while running one.

None of these were visible to a compiler or a test. Two of them were introduced
**in the same session that found the others**.

The counter-practice that worked was mechanical: after a two-node run, read the
**pcap** rather than the transcript. `drive-2vm.py` had been capturing it for
exactly this question since the rig was built, and nothing had ever read it. It
now reports which auth format each node actually used, next to the fault count —
the health bar says the run did not fault, this says what it *did*.

And a stale-evidence trap found while building that: if QEMU never writes a
capture, a previous run's file is still sitting there and the census reports it
with full confidence. The captures are deleted before each run now.

---

## Two smaller things worth keeping

**`clippy` without `--all-targets` never lints tests.** "Clippy clean" was
claimed too broadly twice before this was folded into `make test`, where it is
checked rather than remembered.

**A cross-language constant is a silent failure mode.** `ninep-abi` and the two
Python peers spell the protocol constants independently, with no shared header
possible across that boundary. A drift surfaces as "authentication failed",
which reads as a key problem and sends you to the wrong file.
`scripts/check-wire-constants.py` (in `make test`) compares them, with per-peer
coverage baselines — because a bare "more than zero matched" lets one peer drop
out while the other's matches carry the total. The near-miss that prompted it:
the signature domain tags held a **raw NUL** rather than a `\0` escape. Same
byte, but it made `grep` treat the file as binary and rendered as a trailing
space — one whitespace-stripping hook from silently changing the signed bytes.

---

## What this arc did not do

Stated plainly, because the value of the last two arcs was partly in saying so:

- **Keys are per-machine, not per-user.** An authorized machine can still claim
  any of *its own* users' names. The model defends against the users of a
  trusted node — the real exposure — not against a compromised node. That is
  item 1 on the roadmap, and the auth-server note evaluates the shape it would
  take.
- **`AUTHORIZED_MAX` is 1 KB, about a dozen peers.** One shared secret scaled to
  any number of nodes; a peer list does not. netd warns when the file is bigger
  than it can read, and `clusterkey peers` flags a line that cannot work, so the
  limit is visible rather than silent — but it is a real ceiling that did not
  exist before.
- **No replay protection, no encryption.** Both remain gated behind the
  "leaving a trusted network" trigger. A passive observer still reads your
  files.
- **`clusterkey new` refuses without real entropy**, and neither Parallels nor
  the Raspberry Pi has a virtio-rng. On those, keys must be staged at build
  time. It does not bite yet — the cluster is unreachable on both for want of a
  supported NIC — but it is the constraint that will.

---

## The line to remember

Every finding in this arc — the dead relocation check, the empty vector lists,
the bound that could not be reached, the stacked mutations, the probe in the
wrong slot, the vacuous forgery test, the small-order table — is the same
finding.

**A green check is a claim about the checker, not about the code.** The only
way to learn which you have is to break the code on purpose and watch the check
notice.
