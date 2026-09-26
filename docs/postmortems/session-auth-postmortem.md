# What statelessness was doing

*A design retrospective, the thirty-first, covering 2026-09-22, 2026-09-23 and
2026-09-26: session-scoped authentication, planned in
[`roadmap-session-auth.md`](../roadmap/roadmap-session-auth.md) and carried
through its eight steps in eleven PRs (#157 and #159 to #168; #161 is a side
fix that one of the arc's controls turned up). Before it, every message on a
held 9P session carried its own Ed25519 signature. After it, the `NP_SESSION`
handshake agrees two keys by X25519, and every later message carries a
sequenced HMAC under them. The crypto in a fid verb went from 2,926 µs to 169,
and the arc shipped as v0.21.0.*

The previous postmortem, [`async-rmount-postmortem.md`](async-rmount-postmortem.md),
named this arc as its mirror image. Blocking had been a lock nobody wrote down,
and per-request signing was a statelessness nobody wrote down. It left the same
question for both: before replacing it, list what it was providing for free.
This arc is the answer, and the list turned out to be the whole of the hard
part.

> **STATELESS WAS A GUARANTEE, NOT AN ABSENCE.** A per-request signature needs
> nothing fresh, nothing resident, nothing that survives a reboot, no secret
> beyond the machine key, and no memory of who is speaking. A session key needs
> every one of those, and each is a new mechanism with its own way to fail.
> Before adding state, write down what it must never repeat, where it lives, how
> long it lives and what it is bound to, and give each answer a check.

The cryptography was the part anyone would have called risky, and no finding
against it changed a result: the RFCs' own vectors and a second, independent
implementation stood behind it from its first commit. Everything that went
wrong, in four rounds of plan review and in the reviews of the code, was in the
state.

---

## The arc

| step | PR | what landed |
|---|---|---|
| 0 | #157 | the measurement, and the plan built on it |
| 1 | #159, #160 | X25519; its stack gate failed at the park, and #159 moved every sign into the service pass |
| 2 | #162 | HMAC-SHA-512 |
| 3 | #163 | the boot identity in the kernel: a persisted counter and firmware entropy, syscall `BOOT_ID` |
| 4 | #164 | the keyed wire in `ninep-abi`'s normative block, and the wire checker taught to read it |
| 5 | #165 | keyed sessions in both Python peers, derived independently, with misbehaving modes |
| 6 | #166 | the export keys a session |
| 7 | #167 | the client keys a session |
| 8 | #168 | the documents, and v0.21.0 |

#161 closed the libc delegation race of 2026-09-03, which step 1's control on
`main` reproduced one boot in three. No wire flag day: a node offers a key and
never requires one, and a v0.20.0 peer on either end gets today's signed
session.

## 0. The phrase was a downgrade, and the old number answered another question

The item had been carried as "sign once per session rather than per verb".
Read against the auth spec in `ninep-abi`, that means: verify one signature
when the session opens, then trust a cleartext TCP connection. Anyone on the
path could then inject a request, and today that injection fails at the
signature check. The only version worth building agrees a session key and MACs
every message, which needed two primitives the tree did not have.

The cost had been judged once, on 2026-08-31: about 1.4 ms for a sign and a
verify, small beside the network. That was measured on one-shot connections,
which also pay a TCP connect per request. A held session does not, and nobody
had measured one. Step 0 did, on a throwaway branch, and found **2,926 µs of
crypto in every fid verb**, 44% of the median. The probe was checked against
an observer that shares no code with it: the packet capture's count of signed
frames, the counters and the workload's arithmetic all said 42.

Two lessons, both cheap. Read the words an item is carried under against the
spec before planning it, because a phrase can be carried for a long time without
anyone noticing it describes a weaker system. And an old conclusion is only as general
as the question it was measured on: "the crypto is small beside the network"
was true of a connection that paid a handshake per request, and nobody had
restated it when sessions removed the handshake.

## 1. What statelessness provided, item by item

Each row is a property the signed session had without any code, what a keyed
session needs in its place, and where the gap was found.

| property | per-request signing | a keyed session needs | found by | the guard now |
|---|---|---|---|---|
| **freshness** | nothing to keep fresh: Ed25519 signing is deterministic (chosen for that, since Parallels and the Pi have no entropy), and a signed request stands alone | ephemerals that never repeat, since an export ephemeral repeated for a replayed `NP_SESSION` hands the replay the same `K`; on a node with no entropy, that means a counter that survives every reboot | plan round 1 (the entropy premise), rounds 2 to 4 (the derivation could not be built; a same-boot read-back cannot prove persistence on edk2's RAM variables; two stores could repeat a value); #163's review (a variable that survives a warm reset but not a power-off); #164's review (the spec's input list stopped before the entropy) | a counter counts only if it existed before this boot; every store that held one must take the new value, which is the larger of them plus one; the file is rewritten in place; no usable counter, no keyed sessions |
| **residency** | the signing state lived in a transient frame | keys per connection and per session slot, resident on `serve`'s frame under every chain | step 6's probe (528 bytes, `MAX_CONNS` × `Option<KeyedSession>`); step 7's low-water mark (3 × 144, then 3 × 112) | `Wire`'s fourth const parameter, so the dial and path tables carry zero key bytes; the handshake secret and the session keys are one enum, never live together |
| **secrets in memory** | the machine key, loaded once | an ephemeral scalar held from the service pass to phase two, a shared secret, `K` and two keys | the plan (zero the scalar wherever the slot is released, not only on success); #166's review (an all-zero seed beside the key on a keyless node; a wiping comment that promised more than Rust can); #167's review (the ephemeral left for the reap on a refused opening) | the seed lives inside the identity's `Option`; `Drop` wipes; the comment names what is not wiped; boot entropy is readable by the `CAP_NET` holder alone |
| **identity per message** | every `AUTHNP03` request signed its own user name | a keyed frame carries no name, so a verb runs as whoever opened the session | the plan (Decision 4, written as "a change, not a preservation"); #167's review (sessions are found by uid, so a changed `/etc/passwd` line under a held session would run a verb as a name the caller no longer has) | `SessionAuth::opener`; a caller who resolves to another name is refused `FS_ERR_AUTH`. Checked by reading: no rig changes an account under a live fid |
| **format** | every frame named its own format by its magic | a connection has one format, decided once, and nothing can downgrade it | the plan (the format is decided by the export's signed reply, so it cannot be stripped); #164's review (a second key offer refused only on a keyed session, where the design refuses one mid-stream on any); #166's review (a keyed request not bounded by its declared length) | one connection, one format; an RST and no reply on any authentication failure |

None of these is cryptography. The mechanism that drew the most findings, the
boot counter, appears in no description of session keys anywhere; it exists
here only because two of the three platforms have no entropy, and statelessness
is what had let that fact not matter. The plan's reviews saw the same shape from
the other side
([`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md),
its 2026-09-22 section): the X25519 exchange, the per-direction keys and the
sequenced frame came through four rounds untouched, and the counter drew a
defect in every round after the one that introduced it.

One row runs the other way. Statelessness did not provide replay protection:
any recorded `AUTHNP03` request could be sent again. A strict `seq` closed that
for sessions as a side effect, and one-shot requests stay replayable, so the
cluster roadmap's replay item says which half closed.

The question for next time is the async arc's, turned around. That arc asked
what a wait was serializing before deleting it. Before adding state, ask what
it must never repeat, where it lives, how long it lives, what it is bound to,
and what happens on each path that releases it. Five questions, and each row
above is a finding that answered one of them late.

## 2. Why the crypto held

- **The vectors were the RFCs' own**, parsed from the RFC text and checked
  against Python's `hmac` before use, which is how RFC 4231's test case 3 was
  found to be missing an `=`. A shared misreading of a paraphrase would have
  passed both implementations.
- **Two implementations, kept independent on purpose.** Step 5 could have
  given both Python peers one key-schedule helper. It would have been tidier,
  and it would have made the plan's control impossible: drop an input from a
  shared helper and both sides change together and still agree. Each peer
  derives its own keys, and dropping the request nonce from the client's
  schedule breaks the session at its first keyed verb. An independent
  implementation is a check only while it stays independent, so its
  duplication is the point and must not be refactored away.
- **The foreign observer came first.** The peers were keyed before any guest
  code (step 5 before 6 and 7), so `netd` had something that was not itself to
  be wrong against.
- **What vectors cannot catch was named.** A constant-time compare and the
  all-zero shared secret pass every vector when written wrong. The plan said
  so, each was checked by reading, and each review was told which checks were
  reading and not testing.

## 3. The stack, and where it is recorded

Three stack findings came out of this arc, and each is recorded where its
lesson belongs, in the async postmortem's dated sections: a sign already
running 80 bytes from the guard page at every park, found by a gate that was
asking about X25519 (hence #159, a park never signs); a second caller that
un-inlined `build_9p_reply` and put every signed export verb 1,760 bytes
deeper, with `log`'s console buffers inlined into the keyed dispatch on top;
and the client's resident key state, exactly 3 × 112 bytes of the park path's
headroom.

The point for this postmortem is how they were found. All three came from a gate
the plan required before the step could proceed, and none from a fault.
`STACK_PAGES` stayed 14 across an arc that added two ladders to the export's
handshake. The async arc found its overflows by crashing into them; this one
measured headroom at the call sites first, which is the difference between the
two arcs' stack sections.

## 4. Measuring, with a second observer

Three times a number would have shipped a wrong conclusion, and each time the
correction came from an observer sharing no code with the first:

- the keyed dispatch's frame looked like crypto, and the disassembly showed a
  console call;
- the signed path "was not touched", and the same probe on `main` showed it
  1,760 bytes deeper;
- the re-run timing probe said keyed verbs took 18 ms against step 0's 6.65,
  and the packet capture said replies took 1.4 ms. The probe's own dump sat
  inside the interval it timed.

They are written up in
[`blind-instruments-postmortem.md`](blind-instruments-postmortem.md)'s
2026-09-23 and 2026-09-26 sections, with the instrument defects of the arc's
middle days (a probe that measured its neighbours' buffers, a restore that ran
the mutated bytecode, a check that passed on a crash). One practice from step 7
belongs here: `drive-2vm.py`'s census learned `AUTHNP04` **before** the
measurement, because without it a keyed run would have counted three signed
frames and nothing else, and the cross-check that made step 0 trustworthy would
have gone blind in exactly the run it was needed for.

Two guesses at the stack cost were each a build and a boot. The disassembly was
one command. When a number surprises, the cheapest next step is usually a
different observer, not a better guess.

## 5. Where the defects were found

| where | findings acted on | what they were about |
|---|---|---|
| the plan, four review rounds | 34 raised | entropy and the boot counter, almost all; one control that an honest export could never trigger |
| #159 to #162 | none | |
| #163, the boot identity | 9 | persistence, the read-back, the RNG open |
| #164, the wire | 3 | the spec one sentence short, twice, and an em dash |
| #165, the peers | 1 | a check that passed on a crash |
| #166, the export | 8 | secrets in memory, the length bound, a bound defined in two crates |
| #167, the client | 9 | the opener's identity, the resident state's shape |

The gates found the stack, the reviews found the state, and nothing found a
wrong result in the crypto. The rigs found none of the state defects, for the
reason the async postmortem gave: a rig exercises what it was built for, and a
variable that survives a warm reset but not a power-off, or a `passwd` line
edited under a live fid, is not something a rig does unless someone already
suspects it.

My own process defects were the familiar ones. Twice an uncommitted fix was lost
to git: on 2026-09-23 under a control mutation, and on 2026-09-26 to a branch
switch followed by `reset --hard`. And one `git add -A` committed an empty
`qemu-int.log` from a `drive-qemu.py --help` that took `--help` for an image
path. Review caught the last; the first two cost a re-apply each.

## What it does not give

Stated in the plan and kept here so nobody has to rediscover them:

- **One-shot requests stay replayable**, and encryption and `cpu`-stream reply
  authentication stay trigger-gated behind leaving a trusted network.
- **Forward secrecy holds only for a session whose two nodes both had
  entropy.** QEMU's edk2 offers `EFI_RNG_PROTOCOL`; whether the Pi 4's and
  Parallels' firmware do is still to be measured, and on a node without it the
  machine key alone recovers past session keys.
- **A rollback replays.** A snapshot revert, or rebuilding an image, restarts
  the counter under the same key. Acceptable for a dev rig, never for a
  deployed node.
- **Per-user keys are unchanged.** An authorized machine can still claim any of
  its own users' names.

## What to carry forward

- **Before replacing statelessness, list what it provided**: freshness,
  residency, secrets in memory, per-message identity, format. Give each a guard
  and a check. Section 1 is the cost of answering them late.
- **The mechanism nobody names in the design sketch is where the defects will
  be.** Here it was the boot counter, which exists only because the platforms
  lack entropy.
- **An independent implementation is a check only while it is independent.**
  Do not share code between the implementations a control depends on.
- **Read the words an item is carried under against the spec, and measure the
  premise before building on it.**
- **A surprising number needs a second observer** that shares no code with
  the first.

The next frontier item is per-user keys, and this arc left the seam it will
attach to. A keyed session is now bound to one name, its opener, and today the
only thing vouching for that name is the machine that signed the `NP_SESSION`.
Per-user keys would change who vouches, not the binding.
