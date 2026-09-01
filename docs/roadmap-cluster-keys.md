# Per-machine keypairs — design

The design for the next tier of cluster security after
[v0.15.0](release-notes/v0.15.0.md): **each machine gets its own Ed25519
keypair**, and a machine authorizes its peers by listing their *public* keys.
The shared `\CLUSTER.KEY` goes away.

This is item 1 of [`roadmap.md`](roadmap.md) — the half of it that should be
built first, ahead of both per-*user* keys and the designated auth server
evaluated in the same item.

## What v0.15.0 left

The export authenticates a **machine** with one shared secret, and every machine
holds the same copy. Two consequences:

- **Members are interchangeable.** Any node holding the key is fully trusted; a
  node cannot be added or removed without re-keying every other node, and there
  is no per-peer access control.
- **A peer can claim any user name.** v0.15.0 made a remote request carry *who*
  is asking, but the name is only as trustworthy as the machine asserting it, and
  every machine holds the key that vouches for it.

This design fixes the first and narrows the second. It does **not** finish the
second — see "what this still does not do".

## Why not symmetric per-machine keys

The obvious cheap version — machine A has key `K_A`, and every peer that accepts
A holds a copy of `K_A` — was considered and **rejected**, because it does not
deliver the property it appears to:

> With a symmetric key, **verification capability equals signing capability.**
> If B holds `K_A` in order to verify A, then B can also *sign as A*. In a
> three-node cluster, C can impersonate A to B.

It also gives no containment: a stolen disk yields every key that node holds,
which is every key it verifies with. What it does buy — per-peer revocation and
an allow-list — is real but is a strict subset of what the asymmetric version
buys, at almost the same plumbing cost. The only place it is genuinely sound is a
two-node cluster, where there is no third party to impersonate to, and building
for exactly two nodes is not a foundation.

Recorded here rather than in a commit message because "why not the cheap one" is
the first question this design will be asked.

## Why Ed25519

- **Deterministic signing.** No per-signature randomness. This is decisive here:
  hardware entropy is absent on Parallels and the Raspberry Pi (no virtio-rng),
  and an ECDSA-style scheme leaks its private key outright if a nonce repeats.
  Ed25519 derives its nonce from the key and message by hashing.
- **Small fixed sizes** — 32-byte keys, 64-byte signatures — which matters for a
  wire header and for `netd`'s buffers.
- **One extra primitive.** It needs SHA-512 and nothing else; the tree already
  has hand-rolled SHA-256 and HMAC (`programs/servers/netd/src/hmac.rs`,
  NIST/RFC-validated), so the shape is familiar.
- **RFC 8032 ships complete test vectors**, which is what makes hand-rolling a
  curve survivable in a project with no crypto dependencies.

Not RSA (needs bignum arithmetic, far larger); not ECDSA (needs a good RNG per
signature — see above); not X25519 alone (key agreement, not identity).

## Decision 1 — sign every request; no sessions

The export is **one TCP connection per request**. There is no session to amortize
a handshake over, and inventing one would need per-peer state in `netd` — the
task with no heap, no mutable statics, and a 32 KB stack that has hit its guard
page five times.

So every framed request carries a signature, exactly where today's MAC sits. The
cost is one sign and one verify per operation, against a network round trip that
already dominates them. This keeps the design as stateless as the one it
replaces; **step 5 measures whether that cost is real** before anything is wired.

## Decision 2 — the wire

Today's authenticated request is:

```
[len:4][magic:8][nonce:16][name:32][mac:32][np…]          (88-byte header)
```

It becomes:

```
[len:4][magic:8][nonce:16][name:32][pubkey:32][sig:64][np…]   (152-byte header)
```

- `pubkey` is the **sending machine's** public key — its identity, offered the
  way SSH offers a key. The exporter looks it up in its authorized list *first*
  and refuses an unknown key before verifying anything.
- `sig` is Ed25519 over `nonce ‖ name ‖ np`. The public key is not inside the
  signed bytes: the verification equation already binds a signature to the key
  that verifies it, and the key's *authority* is decided by the lookup, not by
  the signature.
- `name` keeps its v0.15.0 meaning — the requesting **user**, resolved on the far
  side through its own `/etc/passwd`.
- The magic advances again, which makes this the second deliberate flag day. A
  v0.15.0 peer is refused rather than misparsed.

The reply gains a signature in place of its MAC, over `request_nonce ‖ [status]
[result]`, signed by the **exporter's** key.

## Decision 3 — who verifies what, in each direction

These are not symmetric, and conflating them is how a design like this goes
wrong:

- **Inbound** (an exporter verifying a client): the client *offers* its public
  key in the frame. The exporter accepts it only if that exact key is in its
  authorized list. Identity comes from the key, not from an address.
- **Outbound** (a client verifying the exporter's signed reply): the client must
  check against the key it *expects for the host it dialed* — otherwise it would
  accept a reply signed by any cluster member, which is a different and much
  weaker claim. So the authorized list maps **address → key**, the way SSH's
  `known_hosts` does.

Consequently one file serves both roles:

```
/etc/cluster/authorized     <machine-name> <ipv4> <pubkey-hex>
```

Revocation is deleting a line. Adding a node is appending one. There is
**no trust-on-first-use**: an unknown key is refused, never learned.

## Decision 4 — key generation demands real entropy

A machine's private key is 32 random bytes, and a guessable one is worse than no
cryptography at all, because it looks like security. So:

- On-device generation **requires** the `RANDOM` syscall (virtio-rng) and
  **refuses** without it. This is deliberately stricter than the `accounts`
  crate's `salt_from`, which degrades to a clock-derived value and says so
  loudly — "loudly" is adequate for a password salt and not for a machine
  identity.
- Images get their keys from a **host-side generator** at build time, the way
  `scripts/mkpasswd.py` already stages `/etc/passwd`, so the QEMU rigs and the Pi
  cards come with real keys regardless of guest entropy.
- The private key lives at `/etc/cluster/id`, mode **0600** — enforced on ext2,
  and (as with `/etc/shadow`) unenforceable on FAT32, which the boot warning
  already says out loud.

## Decision 5 — lookup tables must stay plain scalars

Any precomputed table in the implementation must be an array of plain integers,
never of pointers, slices or references. A table of references emits
`R_AARCH64_ABS64` relocations, which this loader cannot process — the recurring
PIE trap, in its newest possible disguise. Every step checks the relocation count.

## The steps

Each step is one PR, small enough to review, with a stated way it could fail.
**Steps 1–4 touch no existing code**: they build a pure crate beside the tree, so
if the curve arithmetic does not come together, nothing has been built on it.

| # | Lands | Verified by | Negative control |
|---|---|---|---|
| 1 | SHA-512 in a new pure `ed25519` crate | NIST vectors as host tests | flip a constant, watch vectors fail |
| 2 | Field arithmetic mod 2²⁵⁵−19 | host tests vs reference values generated by a **Python** script using arbitrary-precision ints | `a · a⁻¹ = 1` over random inputs |
| 3 | Curve points + scalar multiplication | derive public keys from RFC 8032 secret keys and compare | one wrong limb changes the key completely |
| 4 | **Sign/verify — the go/no-go gate** | every RFC 8032 §7.1 vector + cross-check against an independent Python implementation | tampered message, signature and key must each be rejected |
| 5 ✅ | It runs on the **guest** (`/bin/edtest`) | same vectors pass on-device; **3,392 B stack, ~1.4 ms sign+verify**; zero `ABS64` in the linked binary (`RELATIVE` entries are present and expected — the loader applies those) | probe calibrated against a known 4 KB frame, so a silently-dead measurement cannot pass; relocations checked by `scripts/check-relocs.sh`, which fails if the tool reports none at all |
| 6a ✅ | The **format**, as a pure `clusterkeys` crate | host tests; refusals tested harder than acceptances | mutation testing found `#node-a` (no space) parsing as a peer **holding a valid key** — commenting a line out has to revoke it |
| 6b ✅ | Host **generator** + image staging | `/etc/cluster/{id,id.pub,authorized}` on the guest, `id` at **0600**; the Rust parser reads the Python generator's *verbatim* output as a host fixture; export unchanged | nothing reads the files yet, so a format error shows up as a file you can `cat` rather than a failed handshake |
| 6c ✅ | **On-device** key generation (`/bin/clusterkey`) | refuses without real entropy — verified by booting a guest with the entropy device *removed*; the existing identity survives the refusal | also refuses to replace an identity without `-f`, and reports unreadable `authorized` lines, which a lookup cannot |
| 7 ✅ | Exporter **accepts** signed frames alongside old ones | **the Python peer signs and the guest verifies** — the wire proven by a foreign implementation before a guest client exists; MAC'd frames unregressed; 25 signed ops with **no supervisor restart**, which is the only signal a userland panic gives | unauthorized key, tampered message, tampered signature, a *different authorized* key swapped in, and a truncated frame — each refused |
| 8 ✅ | Client **sends** signed frames | two-node matrix both directions, **and the captured link carries only `AUTHNP03`** — a run that succeeds proves the cluster works, not that it did so the way you believe | a machine with no `/etc/cluster/id` falls back to the MAC'd format and still serves, verified by removing the file from an image |
| 9 ✅ | Reply signing (mutual auth over the new primitive) | Python verifies guest replies **and signs its own**, so both halves of the exchange are checked by an independent implementation; a client verifying against a *different authorized peer's* key is refused | the shared key now authenticates nothing — step 10 deletes it |
| 10 | **Flag day**: drop the shared key everywhere | old-format peer refused; keyless node fail-closed; **revocation works** — delete a line, the peer stops being served | |
| 11 | Docs, postmortem, release | | |

## What step 5 measured (2026-08-31)

The three deferred questions, answered on the guest by `/bin/edtest` rather than
estimated:

| | measured | what it settles |
|---|---|---|
| **Peak stack**, sign + verify | **3,392 bytes of 32,768 (10%)** | Risk 1 is closed. `netd` has the same 32 KB a `/bin` program does, so the largest computation it would ever do leaves 90% headroom. |
| `sign` with a cached key | ~504 µs | The `SigningKey` form is worth having: 1.7× cheaper than the one-shot. |
| `sign`, one-shot | ~858 µs | It derives the public key every call. Fine for a one-off; not for per-frame signing. |
| `verify` | ~924 µs | Sign + verify is ~1.4 ms per request under emulation. |
| key expansion | ~418 µs | Paid once at boot, not per frame. |

**These are QEMU/TCG numbers — emulated, and pessimistic against real silicon by
roughly an order of magnitude.** They are a ceiling, which is the useful
direction: if the ceiling is acceptable, the floor certainly is.

**Two decisions this settles, both in favour of the simpler code:**

- **The bit-by-bit scalar reduction stays.** It was written as the obvious-but-slow
  choice with an explicit note that step 5 would decide. At ~1.4 ms for a full
  sign-and-verify *under emulation*, against a TCP round trip that dominates it,
  the fast ref10 chain would buy nothing measurable — and would cost a page of
  constants that cannot be checked against anything but another implementation.
- **No fixed-base table.** Same reasoning, plus a table is binary size and a
  standing invitation to the relocation trap this crate has a house rule about.

**A correction, and the reason the relocation check is now a script.** Step 5
originally recorded "zero `ABS64` *and* zero `RELATIVE`". The second half was
false: the linked binary has four `RELATIVE` entries, which is normal — the
loader exists to apply them. The claim survived because the command behind it,
`llvm-readelf -r`, prints **nothing at all** for these binaries, so a `grep -c`
against it returned 0 and was read as "none found" rather than "nothing was
examined". That check could not have failed, in an arc where every other claim
was mutation-tested. It had been used since step 1, so five steps rested on it.

`scripts/check-relocs.sh` replaces it: `llvm-readobj`, across every userland
binary, refusing `ABS64` — and **failing outright if the total `RELATIVE` count
is zero**, because that means the tool has stopped reporting rather than that the
binaries are clean. The ABS64 conclusion itself was re-verified and holds: 55
binaries, 0 `ABS64`, 852 `RELATIVE`.

The stack figure is trustworthy because the instrument was calibrated: the same
probe run around a function with a deliberate extra 4 KB frame reads 4,160 bytes
higher. A probe that silently measured nothing would have reported a small,
plausible number, and a plausible number is exactly what this step must not
accept on faith.

## A consequence found while staging (step 6b)

**The two-node rig can no longer boot one image twice.** It did, for as long as
the cluster shared one symmetric key: `run-image-2vm-ext2-a` and `-b` each copied
`espext2.img`. Per-machine keypairs make that impossible — the two nodes must
hold *different* private keys — so each is now a full rebuild with its own
`CLUSTER_NODE`, and `make images-2vm-ext2` builds the pair. Only
`/etc/cluster/id` differs between them; `authorized` is identical, because every
node accepts the same peers.

The dev keys are **deterministic**, derived from fixed seed strings rather than
generated randomly per build. That is not laziness: A's disk and B's disk are
built by separate `make` invocations, and random keys would give them
disagreeing `authorized` files — a cluster that fails to talk to itself, with a
symptom (authentication refused) that looks nothing like the cause (the images
disagree about who the peers are). It does mean **the dev private keys are in the
repository**, the same trade the existing dev `CLUSTER_KEY` already makes; a real
deployment uses `--random` or generates on the device.

## What step 7 settled

The exporter verifies signatures now, and the thing that verified *it* was a
Python signer — no guest client existed yet, which is the whole reason the step
was ordered this way. A format both ends of which are mine would have proven
only that I am consistent.

**netd's stack held.** The 1 KB `authorized` buffer lives in `Auth` on `serve`'s
frame and an Ed25519 verification runs inside `handle_9p`, on the server whose
32 KB has hit its guard page five times in this project's history. Twenty-five
signed operations produced **zero supervisor restarts** — which is the check that
matters, because `-d int` reads 0 either way: a userland panic *parks* a task
rather than raising a CPU exception, so a guest that "still boots and prints 0
aborts" can have had netd killed and restarted underneath it.

**The order of checks in the verifier is deliberate.** The offered key is looked
up in `authorized` *before* the signature is verified. Partly cost — an unknown
key never gets a scalar multiplication spent on it — but mostly separation: "is
this key allowed here" and "does this signature check out" are different
questions, and a valid signature by a key nobody authorized is exactly as
unwelcome as an invalid one.

## What step 9 settled

Replies are signed by the exporter's private key and verified by the client
against **the key it expects for the address it dialled** — `find_by_ip`, not
`find_by_key`. That asymmetry is Decision 3, and it is the whole reason a line in
`authorized` carries an address as well as a key: an exporter is *offered* a key
and only has to decide whether it is allowed, while a client is offered nothing
and must know in advance whose signature will do. Accepting any authorized
signature would authenticate "some cluster member" when the claim that matters is
"the machine I asked" — any other member could answer for it.

**The reply format follows the request format**, with no discriminator byte: the
exporter knows which kind of request it verified and the client knows which kind
it sent, so a tag would be a field both sides must agree about for information
neither lacks. The corollary is that a machine which cannot sign refuses a signed
request outright, rather than answering with a MAC the caller cannot check.

**A rig failure that looked exactly like a protocol failure.** Signing replies
made the host Python peer ~0.2s slower per request, on top of ~1s to import a
reference whose self-assertions are two full pure-Python signatures. Paid lazily,
that landed on the first client request and pushed it past what the guest's
`tcp_get` waits for — and the guest reported "no filesystem", which reads as a
transport bug. It was a stopwatch. The server warms its signer before accepting
connections now. Worth recording because the diagnosis took several wrong turns:
the symptom pointed at the wire, the packet capture was nearly empty, and only
timing the reference located it.

## Risks, named in advance

1. **`netd`'s stack.** ~~Ed25519 verification is the largest computation this
   server will have done.~~ **CLOSED by step 5's measurement: 3,392 bytes of
   32,768.** The concern was right to raise and turned out not to bind.
2. **Per-op cost.** ~~If sign+verify turns out to dominate a remote
   operation~~ **CLOSED: ~1.4 ms under emulation**, against a TCP round trip that
   dominates it. Decision 1 (sign every request, no sessions) stands, now with a
   measurement behind it.
3. **The flag day.** Step 10 changes every rig, both Python peers, the Makefile
   and the docs at once. It is deliberately last and alone.

## What this still does not do

- **Per-*user* keys.** A machine can still vouch for any of *its own* users, so
  this defends against a machine you have not authorized, not against a
  compromised one lying about its users. That is the tier the designated auth
  server addresses — evaluated in [`roadmap.md`](roadmap.md) item 1.
- **Replay protection, transport encryption, `cpu`-stream reply auth.** Unchanged
  and still gated behind the "leaving a trusted network" trigger in
  [`roadmap-cluster.md`](roadmap-cluster.md).
- **Key distribution.** Public keys are provisioned by hand or at image build.
  With a handful of machines that is honest; it is also exactly the cost that
  makes an auth server attractive later.
