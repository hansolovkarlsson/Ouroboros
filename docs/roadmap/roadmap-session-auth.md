# Session-scoped authentication: the plan

**The auth half of the standup item "session-scoped authentication and an
async `NETOP_RMOUNT`".** `roadmap-async-rmount.md` was the concurrency half and
closed in v0.20.0. This is the plan for the other half, written before any
code. Its first step was a measurement, because whether this arc is worth doing
was a question the tree could answer and nobody had asked it.

Grounded in the code at `3e59cec` (2026-09-22).

## What "sign once per session" would have meant, and why not that

Today every framed export request carries its own Ed25519 signature
(`AUTHNP03`, the normative spec in `ninep-abi`), and every framed reply carries
the exporter's signature bound to the request's nonce. Each message stands on
its own.

The phrase this arc was carried under, "sign once per session rather than per
verb", read literally, means: verify one signature when the session opens, then
trust the TCP connection. **That is a downgrade.** The link is cleartext, so
anyone on the path can inject a segment into an authenticated connection, and
every request after the first would then have no integrity protection at all.
Today that injection fails at the signature check. Nothing in this plan
removes that check without putting an equivalent one in its place.

So the only form of this arc worth building is one where the handshake agrees a
**session key** and every later message carries a MAC under it. That needs two
primitives the tree does not have: **X25519** key agreement and **HMAC**. (The
shared-key HMAC-SHA-256 in `netd` was deleted with `AUTHNP02`.) Both are small:
X25519 runs on the same Curve25519 field arithmetic `ed25519/src/field.rs`
already has, and HMAC is a few lines over the SHA-512 in `ed25519/src/sha512.rs`.

## Step 0: the measurement ✅ 2026-09-22

The cost case was measured once before, on 2026-08-31
(`roadmap-cluster-keys.md` step 5): about 1.4 ms for a sign plus a verify under
emulation, "against a network round trip that already dominates it", so
Decision 1 there (sign every request, no sessions) stood. That comparison was
made against **one-shot** connections, where every request also pays a TCP
connect. A held session does not, so the old number did not settle the new
question.

**How it was measured.** A throwaway branch (`measure/session-auth-cost`,
commit `15a26dd`, kept local and never merged) wraps the four signature calls
in `netd` with `now_us()` and adds the elapsed time to relaxed `AtomicU64`
counters: the client's request sign (`frame_signed_ed25519`), the export's
request verify (`authenticate_signed`), the export's reply sign
(`seal_reply_signed`) and the client's reply verify (`verify_sealed`). A fifth
pair times a session verb end to end on the client, from just before its
request is signed to just after its reply verifies. The counters are printed
from the top of `serve`'s loop, not from inside the calls, because `log()`
carries about 2 KB of buffers and the signature paths are the deepest frames
in the server whose stack has hit its guard page before.

The driver is the client-session witness from `testing-qemu.md`: the ext2
two-node rig, node B mounts node A, and `cbig` runs three times (each run is an
`NP_SESSION`, an open, nine preads and a close over one held session).

**Results, QEMU TCG, `cortex-a72`:**

| operation | count | total | mean |
|---|---|---|---|
| client signs the request | 42 | 21,896 µs | **521 µs** |
| export verifies the request | 42 | 39,594 µs | **943 µs** |
| export signs the reply | 42 | 21,946 µs | **523 µs** |
| client verifies the reply | 42 | 39,444 µs | **939 µs** |
| **all four, per verb** | | | **2,926 µs** |

| session verb, sign to verified reply | n | min | median | p75 | max |
|---|---|---|---|---|---|
| all verbs | 39 | 5,206 µs | **6,654 µs** | 7,156 µs | 53,192 µs |

**The four signature operations are 2.93 ms of a 6.65 ms median verb: 44%.**
On a held session the crypto is the largest single cost of a remote fid verb,
not a rounding error beside the network. Seven of the 39 verbs took 19 to 53 ms;
those are not the crypto (it does not vary by verb) and are not this arc's
question.

**Why the numbers can be trusted.** The probe could have measured nothing and
still printed zeros that looked like "cheap", so it was checked against an
observer it does not share code with: `drive-2vm.py` counts the `AUTHNP03`
magics in each node's packet capture, and reported **42 signed frames on each
node**. The counters counted 42 client signs and 42 client verifies on B, and
42 export verifies and 42 export signs on A. The per-op means also reproduce the
2026-08-31 figure (521 + 939 = 1,460 µs against "~1.4 ms"), measured by a
different tool on a different path. `cbig` passed all three runs, 0 fault
lines on either node.

**What the number does not say.** This is emulation. The crypto is pure CPU
and so is most of the rest of the path (the two `netd`s, `fsd`, the kernel's
IPC), while the "wire" between the two QEMU processes is a localhost socket and
nearly free, so the ratio is plausible for real hardware but not proven for it.
The Pi 4 pair is the next hardware target and has no networking yet, so a
native measurement is not available.

## Decisions

Scored on the project's standing order: **stable, safe, and not blocking
future features.**

**Decision 1: the handshake rides `NP_SESSION`, authenticated by the
signatures that already exist.** The client's `NP_SESSION` request carries a
fresh ephemeral X25519 public key as its payload (32 bytes). It is already an
`AUTHNP03` request, so that payload is covered by the client machine's Ed25519
signature with no new format. The export answers with its own ephemeral X25519
public key as the result (32 bytes), and that reply is already signed with the
exporter's key and bound to the request's nonce. Both sides then compute the
shared secret. **No new signature scheme, no new round trip**: the verb this
project added in September for exactly this purpose ("the natural home for
session-scoped authentication later", `ninep-abi`'s `NP_SESSION` doc) carries
it.

**Decision 2: compatibility is decided by the signed reply, so it cannot be
stripped.** An export that predates this plan ignores `NP_SESSION`'s payload
and answers `0` with an empty result (checked: `netd`'s arm calls
`frame_reply(&mut c.prefix, status, &[])`, the host peer returns
`sealed(0)`). So a new client learns which kind of session it has from the one
thing an attacker cannot alter: 32 bytes of key in the export's signed reply
mean a keyed session, an empty result means today's per-request-signed session.
A client that sends no key (any v0.20.0 node, or the Python client without the
flag) gets today's session from a new export. Both kinds coexist, and a man in
the middle cannot downgrade one to the other without breaking the reply
signature.

**Decision 3: the key schedule.** `K = SHA-512(SIG_DOMAIN_SESSION ‖ shared ‖
request_nonce ‖ eph_client ‖ eph_export)`, then `k_c2s = K[0..32]` and
`k_s2c = K[32..64]`: **one key per direction**, so a MAC made for one direction
can never be presented as the other (the same property the two signature
domain tags give today). The request nonce and both ephemerals are in the
derivation so the keys are bound to this handshake and no other. The ephemeral
private keys are dropped once `K` is derived.

**Decision 4: the keyed frame.** After a keyed `NP_SESSION`, requests are

```
[len:4][magic "AUTHNP04":8][seq:8][tag:32][NP message]         (48-byte header)
```

with `tag = HMAC-SHA-512(k_c2s, seq ‖ NP message)` truncated to 32 bytes (the
RFC 4868 truncation), and replies are

```
[len:4][tag:32][status:8][result]
```

with `tag = HMAC-SHA-512(k_s2c, seq ‖ status ‖ result)`. **`seq` starts at 1
and must be exactly one more than the last**, on both sides. TCP delivers in
order, so any other value is an injection or a replay, and the session is
closed on it (fail-dead, the same rule a failed reply signature follows today).
The user name is no longer in the frame: it was signed into the `NP_SESSION`
request and the export binds the session to it, so a keyed session cannot
change user mid-stream at all (today another user on the session is refused
`FS_ERR_PERM` per request). Once a session is keyed, the export refuses an
`AUTHNP03` frame on it: one connection, one format.

**Decision 5: what stays exactly as it is.** One-shot connections (path verbs,
the mount-time probe) and `NP_RUN` keep per-request `AUTHNP03` signatures.
`cpu`'s output stream stays unauthenticated, as it is today. The per-machine
Ed25519 identity, the `authorized` file, the lookup-by-address rule for the
client and the refuse-if-you-cannot-sign rule are all unchanged. This arc
changes how a session proves each message, not who can open one.

### What it buys, and what it does not

- **Cost.** Per keyed verb, four Ed25519 operations become four HMACs. The arc
  exists because of the step 0 number, so its last step repeats that
  measurement, and the arc is not done unless the crypto share falls.
- **Replay protection on sessions, for free.** A captured keyed request cannot
  be replayed: its `seq` is spent. A replayed `NP_SESSION` gets a new export
  ephemeral, and without the client's ephemeral private key the attacker
  derives nothing. This is part of the "replay protection" item that
  `roadmap-cluster.md` gates behind leaving a trusted network. It arrives here
  as a side effect, not as a reason; **one-shot requests stay replayable**, so
  that item stays open.
- **Forward secrecy** for any later encryption, because the keys come from
  ephemerals. Nothing is encrypted by this plan, and adding encryption later is
  another key off the same derivation, not a new handshake. That is the "not
  blocking future features" test.
- **Not** confidentiality, **not** per-user keys, **not** protection against a
  compromised authorized machine. All unchanged.

### Alternatives rejected

- **Sign once, then trust the connection.** Removes per-message integrity on a
  cleartext link. See the top of this file.
- **Keep per-message signatures, add a server challenge and a sequence
  number.** Replay protection with no new primitives, but it keeps all four
  Ed25519 operations, and the measurement says those are the cost.
- **Static Diffie-Hellman from the machines' Ed25519 keys** (converted to
  X25519). Saves the ephemerals but gives no forward secrecy and uses one key
  for two protocols. The ephemeral costs one scalar multiplication per side per
  session.
- **Encrypt now (ChaCha20-Poly1305).** Tier 2 and trigger-gated in
  `roadmap-cluster.md`. Decision 3 leaves room for it.

## Steps

Each step names the check that proves it and a control that shows the check
can fail. The Rust crates come first, then the wire, then the two Python peers
(the foreign observers), then the export, then the client, so each side is
tested against something that is not itself.

1. **X25519 in `ed25519/`, on the host and on the guest.** RFC 7748 section 5.2
   vectors and the section 6.1 Diffie-Hellman vector as host unit tests. On the
   guest, time one scalar multiplication and measure its stack with
   `/bin/edtest`, calibrated the way step 5 of `roadmap-cluster-keys.md` was.
   **Gate:** its stack must fit where `NP_SESSION` is handled on both sides
   (the export's `handle_9p` path and the client's session opening), which is
   `netd`'s tightest territory. If it does not, stop and re-plan before any
   wire work. *Control:* a flipped bit in one vector fails its test.
2. **HMAC-SHA-512 in `ed25519/`.** RFC 4231 vectors (SHA-512 rows) on the host;
   on the guest, time a MAC over a full `NP_NET_MAX` message. **Gate:** it must
   be well under one Ed25519 sign (521 µs). If a MAC costs what a signature
   does, the arc has no point, and this step is where that is found out.
3. **The wire, in `ninep-abi`.** `NP_AUTH_MAGIC_KEYED` (`AUTHNP04`),
   `SIG_DOMAIN_SESSION`, the header and tag lengths, the `NP_SESSION` payload
   and result shapes, the sequence rule, the key schedule, all in the normative
   block. `scripts/check-wire-constants.py` pins each new constant across Rust,
   the C headers and both Python peers. *Control:* change one constant in one
   peer and the check fails.
4. **Both Python peers.** Pure-Python X25519 (RFC 7748's reference ladder,
   asserted against its vectors on load, as the Ed25519 reference is) and the
   standard library's `hmac`. `np9p_server.py` keys a session when the client
   offers a key; `np9p_client.py` gains a keyed mode. The peer self-test runs
   one keyed session through every verb. This makes a host-to-host keyed
   session work before any guest code changes, so the guest has an
   independent implementation to be wrong against.
5. **The export, in `netd`.** Key a session on an `NP_SESSION` with a payload;
   accept only `AUTHNP04` on it after that. Checked from the host with
   `np9p_client.py`: a keyed `cbig`-shaped run (open, preads, close) is served.
   *Controls, each run:* a tag with one bit flipped is refused and the session
   closes; a replayed `seq` is refused; a skipped `seq` is refused; an
   `AUTHNP03` frame on a keyed session is refused; a reply MAC'd with `k_c2s`
   instead of `k_s2c` is refused by the client. An old-style `NP_SESSION` (no
   payload) still gets today's session. Stack measured at the new peak.
6. **The client, in `netd`.** `park_session` sends the ephemeral, phase two
   derives the keys from the export's signed reply, and later verbs frame
   `AUTHNP04` into the slot. Checked on the two-node ext2 rig: `cbig` and
   `cwrite` pass over a keyed session, and a v0.20.0 export (the unkeyed
   `NP_SESSION` answer) still works from a new client. **Then the step 0
   measurement is re-run with the same instrumentation**, and the plan records
   the new crypto share beside the old 44%.
7. **Docs, rigs and the release.** The normative block, `docs/architecture.md`,
   `docs/manual.md`'s cluster section, `testing-qemu.md`'s session recipes,
   `roadmap-cluster.md`'s replay item (partly closed, and saying which part),
   the changelog, and a minor version.

## Risks, named in advance

1. **`netd`'s stack.** The handshake adds a scalar multiplication to the two
   places that already hold the deepest frames: `NP_SESSION` on the export and
   the session opening on the client. Step 1 measures it before anything is
   wired, and step 5 measures the real peak. The async arc's lesson applies:
   the stack is dominated by resident tables now, and `STACK_PAGES` is held at
   14 on purpose.
2. **Per-session state.** A keyed session carries two 32-byte keys and two
   counters on each side: `TcpConn` on the export, `SessionConn` on the client,
   both resident. That is under 100 bytes per slot, but resident bytes are what
   the stack is short of, so it is counted, not assumed.
3. **Three implementations.** Rust and two Python peers must agree on a key
   schedule and a MAC input byte for byte. A mismatch shows up as a bare
   `FS_ERR_AUTH`, which reads as a key problem. That is why the peers come
   before the guest code (step 4), and why the wire-constant check has to pin
   the domain tag.
4. **Hand-rolled crypto.** X25519 and HMAC here are written from their RFCs,
   as Ed25519 was. The vectors are the defence, plus the Python peers as a
   second implementation from the same RFCs. A shared misreading of the RFC
   would pass both, which is why the vectors are the RFCs' own.
