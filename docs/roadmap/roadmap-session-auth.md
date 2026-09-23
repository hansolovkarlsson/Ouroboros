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
two-node rig, node B mounts node A, and `cbig` runs three times. Each run is
an `NP_SESSION` and then 13 verbs over one held session: an open, eleven preads
(`/man/grep` is 4,661 bytes, so ten 512-byte `NP_REMOTE_CHUNK`s carry data and
one more reads end of file) and a close. Three runs make 39 timed verbs, and
the three `NP_SESSION` requests make 42 signed requests in all. The mount
itself sent no signed frame in this run: the counts leave no room for one.

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

The mean of the same 39 verbs is 11,196 µs (436,660 µs in all).

**The four signature operations are 2.93 ms of a 6.65 ms median verb: 44%.**
Against the mean verb it is 26%, because seven slow verbs pull the mean up.
The crypto itself does not vary by verb, so the figure that cannot be moved by
the choice of statistic is the absolute one, **2,926 µs per verb**, and that is
the number step 7 compares against.
On a held session the crypto is the largest single cost of a remote fid verb,
not a rounding error beside the network. Seven of the 39 verbs took 19 to 53 ms;
those are not the crypto (it does not vary by verb) and are not this arc's
question.

**Why the numbers can be trusted.** The probe could have measured nothing and
still printed zeros that looked like "cheap", so it was checked against an
observer it does not share code with: `drive-2vm.py` counts the `AUTHNP03`
magics in each node's packet capture, and reported **42 signed frames on each
node**. The counters counted 42 client signs and 42 client verifies on B, and
42 export verifies and 42 export signs on A. And 42 is what the workload
predicts (3 + 39, above), so nothing was counted twice or missed. The per-op means also reproduce the
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
shared secret. **No new signature scheme, no new round trip**: the verb chosen
in September partly for this reason ("the natural home for session-scoped
authentication later", `roadmap-fid-verbs.md`'s decision of 2026-09-07)
carries it. `ninep-abi`'s own `NP_SESSION` doc says "No params; no payload"
today, which step 4 changes.

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
derivation so the keys are bound to this handshake and no other.

**An all-zero shared secret closes the connection, on both sides.** A low-order
public key from the peer (RFC 7748 section 6.1) would make `K` depend on public
values only. The export that computes one sends no reply and closes; the client
that computes one from a signed reply closes. There is no fallback to an unkeyed
session: falling back would leave the two ends disagreeing about the format of
the next frame, which surfaces only as a bare `FS_ERR_AUTH`.

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
Tags are compared in constant time (every byte, no early exit). A test vector
cannot catch an early-exit compare, so this one is checked by reading the code,
and the review is told so.

The user name is no longer in the frame: it was signed into the `NP_SESSION`
request, and every verb on a keyed session runs as that user. **This is a
change, not a preservation.** Today only a fid verb checks the user (the export
refuses a fid whose opener's uid differs, `session_slot`); a path verb on a
session runs under whatever name its own request carries. `netd` as a client
already opens one session per `(endpoint, uid)`, so it never mixes users on
one, but the export and both Python peers need the rule written down, and step
6 tests it. Once a session is keyed, the export refuses an `AUTHNP03` frame on
it: one connection, one format.

**What a refusal looks like on a keyed session.** Two kinds, kept apart. A
*verb* that fails (no such file, permission denied, `FS_ERR_BUSY`) is an
ordinary keyed reply: a status under a valid tag, exactly as a success is. An
*authentication* failure at the export (a bad tag, a wrong `seq`, an `AUTHNP03`
frame, a second `NP_SESSION` offering a key) gets **no reply**: the export logs
it and closes the connection with an RST. It cannot answer in a way the client
could trust, since the failure means the export no longer knows it is talking
to the key holder, and an unauthenticated "auth failed" is exactly what an
attacker could forge to kill sessions. So the client sees its parked verb end
with the connection, and reports it as it reports a dead peer today. That
reads as a network fault rather than an auth failure, and it is chosen: the
export's log is where the reason is. A failure the *client* detects (a reply
whose tag or `seq` is wrong) is `FS_ERR_AUTH` to the caller, and the client
closes, as a bad reply signature does today. An `NP_SESSION` with no payload
sent on a keyed session (inside an `AUTHNP04` frame) is answered `0` under a
valid tag, which is today's idempotent arm, unchanged.

**Keying happens once, on the first `NP_SESSION` of a fresh connection.** A
later `NP_SESSION` that offers a key (on a keyed session, or on an unkeyed one
that would be keyed mid-stream) is refused and the connection closed: there is
no re-keying, because a session's life is already bounded by its fids and the
idle reap. A later `NP_SESSION` with no payload on an unkeyed session keeps
today's idempotent `0`, so a v0.20.0 client sees no change.

**Decision 5: what stays exactly as it is.** One-shot connections (the path
verbs) and `NP_RUN` keep per-request `AUTHNP03` signatures. `cpu`'s output
stream stays unauthenticated, as it is today. The per-machine Ed25519 identity,
the `authorized` file, the lookup-by-address rule for the client and the
refuse-if-you-cannot-sign rule are all unchanged. This arc changes how a
session proves each message, not who can open one. (A remote mount sends
nothing when it is made, `cmd_mount_remote` only records it, so there is no
mount-time probe to keep; the one `roadmap-fid-verbs.md` mentions was never
built.)

**Decision 6: where the ephemeral keys come from.** Parallels and the Pi have
no virtio-rng (which is why Ed25519 was chosen for its deterministic signing),
and today's request nonce is the clock plus the IP. A guessable ephemeral lets
an attacker compute the session keys. An export ephemeral that repeats for a
replayed `NP_SESSION` gives the replay the same `K`, and every recorded
`AUTHNP04` frame verifies again. So each ephemeral is hashed from a secret and
from inputs that do not repeat:

- **client:** `SHA-512(SIG_DOMAIN_EPHEMERAL_C ‖ machine private key ‖ boot ID ‖
  MONOTONIC_US ‖ boot entropy ‖ RANDOM bytes)`, reduced to an X25519 scalar.
  It is computed in the service pass, in `sign_parked` (since #159 the one
  place a parked request is signed), and written into the raw `NP_SESSION`
  payload **before** that payload is signed in place, so it does not depend on
  the request nonce, which the signer creates next; the signing API is
  untouched. (This said `park_session` until step 1's gate measured that site
  at 80 bytes from the guard page; see step 1.) The 32-byte private scalar is
  **kept in the session slot** until phase two has derived `K`. It is zeroed
  **wherever the slot is released**, not only on success: a refused or empty
  `NP_SESSION` answer, an unreachable peer, a low-order close, a reap. Zeroing
  on the success path alone would leave the secret resident on exactly the
  paths nobody watches. It is not re-derived: re-deriving would need every
  input kept anyway, including the random bytes.
- **export:** `SHA-512(SIG_DOMAIN_EPHEMERAL_E ‖ machine private key ‖ boot ID ‖
  request nonce ‖ MONOTONIC_US at the handshake ‖ boot entropy ‖ RANDOM
  bytes)`, used in the same `NP_SESSION` arm and never stored.

"Boot entropy" and "RANDOM bytes" are each empty where the platform has none.
The private key makes each ephemeral secret. The **boot ID** makes it differ
across reboots, and it comes from the kernel (step 3): before
`ExitBootServices`, the kernel increments a counter, persists it and reads it
back before handing it out, preferring a **UEFI non-volatile variable** (it
lives in firmware storage, so rolling back the disk image does not roll it
back) and falling back to a file on the ESP. In the same place it asks the
firmware for `EFI_RNG_PROTOCOL` bytes, the **boot entropy**, if the firmware
offers them. A new syscall hands both to userland.

**A counter is trusted only if it existed before this boot.** A read-back in the
same boot proves nothing about persistence: firmware with no real variable
store (edk2 as every QEMU target here boots it, `-bios` and no pflash
variable file) keeps variables in RAM, so a write succeeds and reads back
within the boot and is gone at the next. The rule that catches this without a
second boot is to look before writing: the kernel reads the counter, and only
a value that was **already there** counts. A store that forgets at every reboot
never has one, so it is never trusted, and the kernel falls back to the ESP
file. The ESP file is staged at image build (as `/etc/cluster/id` is), so the
first boot of a new image already finds one.

**The two stores never hand out the same value twice.** The kernel reads
both, takes **the larger of the trusted values, plus one**, and writes that to
both. So when the variable store starts working (its first boot finds no
variable and uses the file), or stops (an NVRAM reset or a firmware update
wipes it and the file serves again), the next value is still above every value
either store has seen, instead of restarting from one store's own history.
**If no store yields a counter
that existed before this boot, and was then incremented, written and read
back, the syscall says so and the node keys no sessions**: its client offers
no key and its export answers with an empty result, which is today's session,
still per-request signed. That is fail-safe, not fail-open.

Two things this does **not** give, stated so nobody has to discover them:

- **Replay after a rollback.** A VM snapshot revert rolls the non-volatile
  variable back with everything else, and the ESP fallback rolls back with the
  disk. **Rebuilding the image is a rollback too**: `make image` and
  `images-2vm*` recreate the disk from scratch and stage the file afresh, and
  the dev keys from `mkclusterkeys.py` are deterministic, so on QEMU every
  rebuild restarts the boot ID under the same machine key. That is acceptable
  for a dev rig and would not be for a deployed node, which is never rebuilt
  in place. After that, on a node with no boot entropy, an export ephemeral differs
  from a recorded one only by the clock reading at the handshake. A replayed
  `NP_SESSION` then reproduces an old `K` only if it is served at the same
  microsecond as the recorded one, and the clock passes each value once per
  boot. Replay within a session stays closed by `seq`. This residual is never
  worse than today, where every one-shot request can be replayed.
- **Forward secrecy without entropy.** On a node with no boot entropy and no
  virtio-rng, the machine's private key **alone** recovers past session keys:
  the boot ID is a small number an attacker can count through, and the clock
  inputs can be searched around each handshake in a capture. So there,
  forward secrecy does not hold, and the plan does not claim it. And because
  `K` can be recomputed from **either** side's ephemeral, it holds for a
  session only when **both** nodes had entropy: a QEMU node with virtio-rng
  talking to a Pi with none has no forward secrecy for that session, since the
  Pi's key alone recovers the Pi's ephemeral and, with the other side's public
  one from the capture, `K`. Whether Parallels' and the Pi's
  firmware offer `EFI_RNG_PROTOCOL` is part of what step 3 measures.

A boot ID would also fix a property of today's `AUTHNP03` nonce, which is the
clock plus the IP and so repeats across reboots. Using it there is a separate
change and not part of this arc.

### What it buys, and what it does not

- **Cost.** Per keyed verb, four Ed25519 operations become four HMACs. The arc
  exists because of the step 0 number, so step 7 repeats that
  measurement, and the arc is not done unless the crypto share falls.
- **Replay protection on sessions, as a side effect.** A captured keyed request
  cannot be replayed: its `seq` is spent. A replayed `NP_SESSION` meets a new
  export ephemeral and derives nothing, except in the rollback case Decision 6
  states. This is part of the "replay protection" item that
  `roadmap-cluster.md` gates behind leaving a trusted network. It arrives here
  as a side effect, not as a reason; **one-shot requests stay replayable**, so
  that item stays open.
- **Forward secrecy** for any later encryption, **only for a session whose
  two nodes both had entropy** (Decision 6). Nothing is encrypted by this plan, and adding
  encryption later is another key off the same derivation, not a new
  handshake. That is the "not blocking future features" test.
- **Not** confidentiality, **not** per-user keys, **not** protection against a
  compromised authorized machine. All unchanged.

### Alternatives rejected

- **Sign once, then trust the connection.** Removes per-message integrity on a
  cleartext link. See the top of this file.
- **Keep per-message signatures, add a server challenge and a sequence
  number.** Replay protection with no new primitives, but it keeps all four
  Ed25519 operations, and the measurement says those are the cost.
- **Static Diffie-Hellman from the machines' Ed25519 keys** (converted to
  X25519). Saves the ephemerals, but uses one key for two protocols and has no
  forward secrecy even on a node that has entropy.
- **Encrypt now (ChaCha20-Poly1305).** Tier 2 and trigger-gated in
  `roadmap-cluster.md`. Decision 3 leaves room for it.
- **The boot counter as a file `netd` writes.** It would be `netd`'s first
  write path, and it rolls back with the disk. The kernel reaches the same
  counter before any task runs, can prefer firmware storage, and can ask the
  firmware for entropy in the same place.
- **No boot counter at all** (the clock and the boot-time physical counter
  only). Nothing to persist, but a repeat across reboots becomes unlikely
  rather than impossible, which is the property the counter exists to give.

## Steps

Each step names the check that proves it and a control that shows the check
can fail. The crypto comes first, then the kernel's boot identity, then the
wire, then the two Python peers (the foreign observers), then the export, then
the client, so each side is tested against something that is not itself.

1. ✅ **2026-09-23.** **X25519 in `ed25519/`, on the host and on the guest.** RFC 7748 section 5.2
   vectors and the section 6.1 Diffie-Hellman vector as host unit tests. On the
   guest, time one scalar multiplication and measure its stack with
   `/bin/edtest`, calibrated the way step 5 of `roadmap-cluster-keys.md` was.
   **Gate:** its stack must fit where the shared secret is computed on each
   side, which is not where the session opens. On the client that is **phase
   two in `service_remotes`**, the pump shared by the one-shot and session
   tables, where the reply's signature verify, the scalar multiplication and
   the key derivation run back to back. On the export it is the `NP_SESSION`
   arm under `handle_9p`, which also computes the export's own ephemeral. And
   `park_session`, which computes the client's ephemeral: a Montgomery ladder
   costs the same stack whatever the point, so a base-point multiply is not
   cheaper there. The gate adds today's measured peak at all three call sites
   to the new cost. If it does not fit, stop and re-plan before any wire work.
   *Control:* a flipped bit in one vector fails its test.

   **What it measured, and the re-plan it forced.** The crypto passed as
   planned: every RFC 7748 vector (the million-iteration row in release), three
   exchanges generated by OpenSSL 3.6.4 on random keys, and small-order refusal;
   the control fails three tests. On the guest, one scalar multiplication is
   **236 µs** (a cached sign is 564) and **2,944 bytes** of stack, both probes
   calibrating at +4,176. **The gate failed, at `park_session`, and not because
   of X25519.** A probe in `netd` calibrated with a 4 KB pad (+4,208) ran a sign
   and an X25519 at each site on the two-node ext2 rig (branch
   `measure/x25519-stack-gate`, never merged):

   | site | today's sign: headroom | X25519: headroom |
   |---|---|---|
   | `park_session` | **80** | 240 |
   | phase two in `service_remotes` | 10,400 | 10,560 |
   | export `NP_SESSION` arm | 10,400 | 10,560 |

   A 512-byte pad at the `park_session` depth faulted `netd` at its guard page,
   which is the control that the figure is real. X25519 was no deeper than the
   sign already there, which ran at 80 bytes on every fid verb on `main`: every
   park is reached from `drain_client_messages` inside the export's connection
   pump, ~10 KB below the service pass. So the plan stopped, as it says, and
   the re-plan was **#159: a park never signs.** It writes its request raw and
   `service_remotes` signs it in place in `sign_parked`, which is where the
   client's ephemeral now goes. Re-measured after (branch
   `measure/x25519-stack-gate-after`): `sign_parked` has **10,352** bytes with
   X25519, phase two and the export arm 10,560, all three calibrated. The gate
   passes. The sites in the step's text above are as planned; the ones to
   build at are `sign_parked`, phase two, and the export arm.
2. ✅ **2026-09-23.** **HMAC-SHA-512 in `ed25519/`.** RFC 4231 vectors (SHA-512 rows) on the host;
   on the guest, time a MAC over a full `NP_NET_MAX` message. **Gate:** it must
   be well under one Ed25519 sign (521 µs). If a MAC costs what a signature
   does, the arc has no point, and this step is where that is found out.
   *Control:* a flipped bit in one vector's key, message or expected tag fails
   its test.

   **What it measured.** All seven RFC 4231 SHA-512 rows pass (parsed from the
   RFC text, each checked against Python's `hmac` first; the RFC's own case 3
   is missing the `=` after `Key`), plus keys of exactly 128 and 129 bytes,
   every split of a streamed message, and every flipped bit of a key and a
   message. The control: a flipped bit in case 3's key, its data, and its
   expected tag each fails the test. `ct_eq` has no early exit, checked by
   reading it. On the guest, one MAC over the sequence number plus a full
   `NP_NET_MAX` message is **120 to 160 µs** across three runs, against **536
   µs** for a cached sign in the same runs: at most 30% of a sign at the
   largest message there is. **The gate passes.** Four full-size MACs are at
   most ~640 µs against step 0's 2,926 µs for the four signature operations,
   and most verbs are far smaller than `NP_NET_MAX`. Its stack is 5,264 bytes
   including the 2,096-byte message the probe holds, so ~3.2 KB of its own.
   Measuring it also caught a fault in `edtest` itself: with the three
   operations as arms of one function, the HMAC arm's buffer inflated the
   other two readings by up to 5 KB, because the probe reads depth from the
   top of the stack; each now has its own frame, and sign+verify reads its
   historical 3,584 again (X25519 now reads 2,784, the 2,944 above having
   carried the old dispatch frame).
3. **The boot identity, in the kernel.** Before `ExitBootServices`: increment,
   persist and read back the counter (a non-volatile variable where the
   firmware keeps one across a reboot, else a file on the ESP), and ask for
   `EFI_RNG_PROTOCOL` bytes; the image build (`make image`, `images-2vm*`)
   stages the ESP counter file. A new syscall returns the counter, whether it was
   persisted, and the boot entropy if any. **Check:** two consecutive QEMU
   boots **of the same image file**, driven by `scripts/drive-qemu.py` against
   `build/esp.img` and not through `make run-image` (which is `.PHONY`,
   depends on `image`, and recreates the disk and the staged counter every
   time, so it would report the same counter twice and blame the kernel for
   the build), report two different counters, and the one after reads back the
   value the one before wrote. QEMU as the Makefile boots it has no pflash
   variable file, so this is also the check that the RAM-only variable store
   is refused and the ESP fallback is what serves. **Measured and recorded:** which store works
   and whether `EFI_RNG_PROTOCOL` exists, on QEMU now and on the Pi 4 and
   Parallels when each can be booted; the design has to work with "neither".
   *Controls:* skip the increment and the check fails (the same counter
   twice); make the persist fail and the syscall reports the counter unusable;
   remove the "existed before this boot" test and the RAM-only variable store
   is trusted, which the two-boot check then catches as a repeated counter.
4. **The wire, in `ninep-abi`.** `NP_AUTH_MAGIC_KEYED` (`AUTHNP04`),
   `SIG_DOMAIN_SESSION`, the two ephemeral domain tags, the header and tag
   lengths, the `NP_SESSION` payload and result shapes, the sequence rule and
   the key schedule, all in the normative block, and the `NP_SESSION` doc's
   "No params; no payload", which stops being true.
   **`SIG_DOMAIN_MAX` is extended over every tag**, the three new ones as well
   as the two it covers today: `netd` sizes its signing prefix buffers as
   `SIG_DOMAIN_MAX + …`, and a longer tag hashed through one of them would be a
   slice-index panic in the `NP_SESSION` arm, reachable before authentication.
   `scripts/check-wire-constants.py` already parses a `u64` hex constant on the
   Rust side, so the gaps are elsewhere: **`AUTHNP03` itself is not in its
   checked list today**; the Python peers spell a magic as
   `int.from_bytes(b"AUTHNP03", "big")`, which it does not parse; and a length
   written as a sum does not parse **on either side** (the Rust parser matches
   only `usize = <digits>`, and `NP_AUTH_HDR_SIGNED` is a sum across two
   lines). This step teaches it both forms, on both sides, and pins both
   magics and both header lengths. The C headers carry no
   auth constants and gain none. *Control:* for each form the parser learns,
   change one constant in one peer and the check fails.
5. **Both Python peers.** Pure-Python X25519 (RFC 7748's reference ladder,
   asserted against its vectors on load, as the Ed25519 reference is) and the
   standard library's `hmac`. `np9p_server.py` keys a session when the client
   offers a key; `np9p_client.py` gains a keyed mode. The peer self-test runs
   one keyed session through every verb. This makes a host-to-host keyed
   session work before any guest code changes, so the guest has an
   independent implementation to be wrong against. *Control:* give the client
   and server peers different key schedules (one input dropped from one side's
   derivation) and the self-test fails on its first keyed verb; and
   `np9p_client.py` refuses each of the server's misbehaving replies below,
   including one tagged with `k_c2s` instead of `k_s2c`. (That control used to
   sit under step 6, where the export under test is an honest `netd` that never
   sends such a reply, so it could not fail there.) The server
   also gains **misbehaving modes** for step 7 to aim at: a flipped reply tag,
   a reply tagged with the wrong direction's key, and a replayed or skipped
   reply `seq`.
6. **The export, in `netd`.** Key a session on an `NP_SESSION` with a payload;
   accept only `AUTHNP04` on it after that. Checked from the host with
   `np9p_client.py`: a keyed `cbig`-shaped run (open, preads, close) is served.
   *Controls, each run:* a tag with one bit flipped is refused and the session
   closes; a replayed `seq` is refused; a skipped `seq` is refused; an
   `AUTHNP03` frame on a keyed session is refused; a low-order ephemeral closes
   the connection; a second `NP_SESSION` offering a key is refused and the
   connection closes; a path verb on a keyed session runs as the session's
   user, not a name of its own. An old-style `NP_SESSION` (no payload) still
   gets today's session, and a node whose boot counter is unusable keys
   nothing. Stack measured at the new peak.
7. **The client, in `netd`.** The service pass (`sign_parked`, before it signs
   the raw `NP_SESSION`) computes the ephemeral and sends its public half, phase two derives the keys from the export's signed reply
   and zeroes the private scalar, and later verbs frame `AUTHNP04` into the
   slot. Checked on the two-node ext2 rig: `cbig` and `cwrite` pass over a
   keyed session, and a v0.20.0 export (the unkeyed `NP_SESSION` answer) still
   works from a new client. *Controls, each run, aimed at `netd`'s client (not
   `np9p_client.py`):* `np9p_server.py`'s misbehaving modes from step 5, one at
   a time (a flipped reply tag, a reply tagged with `k_c2s`, a replayed reply
   `seq`, a skipped one), and each must end the caller's verb with
   `FS_ERR_AUTH` and close the session; `cbig` against an honest export in
   the same run must still pass. Without these, a client that accepted any
   reply on a keyed session would pass every other check in this step. Stack
   measured at the client's new peak. `drive-2vm.py`'s auth-format table learns
   `AUTHNP04` in this step, before the measurement: it knows only `AUTHNP03`
   and `AUTHNP02`, so on a keyed run it would count the three `NP_SESSION`s and
   nothing else, and the cross-check that made step 0 trustworthy would go
   blind (the observer is the check nobody updates). **Then the step 0
   measurement is re-run with the same instrumentation**, checked against that
   count. **Done means the crypto per keyed verb, in absolute µs, is well
   below 2,926 µs**; the plan also records the new median and mean shares
   beside the old 44% and 26%, the same statistic against the same statistic.
8. **Docs, rigs and the release.** The normative block, `docs/architecture.md`
   (the new syscall and the boot identity), `docs/manual.md`'s cluster section,
   `testing-qemu.md`'s session recipes, `roadmap-cluster.md`'s replay item
   (partly closed, and saying which part), the changelog, and a minor version.

## Risks, named in advance

1. **`netd`'s stack.** The handshake adds a scalar multiplication to three
   places that already hold deep frames: the `NP_SESSION` arm on the export
   (two, counting the export's own ephemeral), phase two in `service_remotes`
   on the client, right after the reply's signature verify, and the client's
   ephemeral, which step 1's gate moved from `park_session` (80 bytes of
   headroom, measured) to `sign_parked` in the service pass (#159).
   Step 1 measures it before anything is wired, and steps 6 and 7 measure the
   real peak. The async arc's lesson applies: the stack is dominated by
   resident tables now, and `STACK_PAGES` is held at 14 on purpose.
2. **Per-session state.** A keyed session carries two 32-byte keys and two
   sequence counters on each side, `TcpConn` on the export and `SessionConn`
   on the client, and the client's slot also holds the 32-byte ephemeral
   scalar while the session opens. All resident. The client's `SessionConn`,
   `RemoteConn` and `DialConn` (the `/net/tcp` table) are the **same generic
   `Wire`**, so a field added for sessions lands in every `MAX_REMOTE` and
   every dial slot too, unless it is placed where only the session table
   carries it. Around 100 bytes per slot, but resident bytes are what the
   stack is short of, so where it lives and what it costs are counted in steps
   6 and 7, not assumed.
3. **Three implementations.** Rust and two Python peers must agree on a key
   schedule and a MAC input byte for byte. A mismatch shows up as a bare
   `FS_ERR_AUTH`, which reads as a key problem. That is why the peers come
   before the guest code (step 5), and why the wire-constant check has to pin
   the domain tags.
4. **Hand-rolled crypto.** X25519 and HMAC here are written from their RFCs,
   as Ed25519 was, including the two checks such code most often leaves out:
   the all-zero shared secret and the constant-time tag compare. The vectors
   are the defence, plus the Python peers as a second implementation from the
   same RFCs. A shared misreading of the RFC would pass both, which is why the
   vectors are the RFCs' own.
5. **Firmware.** Variable services and `EFI_RNG_PROTOCOL` differ by firmware.
   The Pi 4's community UEFI keeps its variables in a file on the SD card, so
   there a variable rolls back with the card; Parallels is parked and untested
   here. Step 3 records what each platform does, and the design's fail-safe
   (no persisted counter, no keyed sessions) is what runs where neither store
   works.
