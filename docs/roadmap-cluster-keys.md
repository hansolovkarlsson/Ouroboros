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
| 5 | It runs on the **guest** | same vectors pass on-device, byte for byte; **stack and time measured** with `MONOTONIC_US`; zero `ABS64` | too slow or too much stack = redesign before wiring, not after |
| 6 | Key files + generator, **no wire change** | parse round-trips as host tests; correct modes on-device; export still behaves exactly as before | nothing reads the new files yet |
| 7 | Exporter **accepts** signed frames alongside old ones | **the Python peer signs and the guest verifies** — the wire is proven by a foreign implementation before a guest client exists | wrong key, unknown key, tampered frame each refused |
| 8 | Client **sends** signed frames | two-node matrix both directions; Python verifies guest signatures | |
| 9 | Reply signing (mutual auth over the new primitive) | Python verifies guest replies | tampered reply rejected |
| 10 | **Flag day**: drop the shared key everywhere | old-format peer refused; keyless node fail-closed; **revocation works** — delete a line, the peer stops being served | |
| 11 | Docs, postmortem, release | | |

## Risks, named in advance

1. **`netd`'s stack.** Ed25519 verification is the largest computation this
   server will have done. Step 5 measures it in isolation; if it does not fit,
   the answer is a redesign (a smaller table strategy, or moving verification
   somewhere with room), not a bigger stack bolted on at step 8.
2. **Per-op cost.** If sign+verify turns out to dominate a remote operation, the
   session question from Decision 1 reopens — with a measurement behind it
   instead of a guess.
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
