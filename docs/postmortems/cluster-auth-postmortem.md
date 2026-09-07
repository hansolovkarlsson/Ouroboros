# Cluster authentication — the export-hardening postmortem (the fourteenth)

*A design retrospective, 2026-08-26 — the day the distributed cluster stopped
trusting the whole LAN. Phases 1–4 all shipped with the same loud asterisk:
**trusted-LAN, no authentication**. Any host that could open a TCP connection to
a machine's 9P export (port 564) could read and write its disk (`mount -r`) and
run arbitrary `/bin` programs on it (`cpu`). This is the day that door got a
lock. Companion to the [distributed-cluster
postmortem](cluster-distributed-postmortem.md) — that one built the door; this
one is about locking it without rebuilding it.*

The output isn't a new subsystem. It's a gate at one chokepoint, a hand-rolled
HMAC, and a shared secret — plus the discipline of proving it against something
that didn't share our bugs.

## The shape of the problem

Every distributed entry point funnels through **one function**: `netd`'s
`handle_9p`, which serves a framed `ninep-abi` request arriving on the export
TCP connection. An fs verb reads/writes the disk; `NP_RUN` spawns a `/bin`
command. Both were served to anyone who connected. So the whole hardening job
was: *authenticate at that one chokepoint, and at the client that reaches it.*
No new server, no new task, no protocol redesign — the security debt was
concentrated exactly where the distributed convergence had already concentrated
everything else (one uniform verb set, one resolver, one gateway). The Phase-0
insight — "remote is the same protocol over TCP" — paid off a second time here:
*one protocol has one place to add auth.*

## The decision that set the ceiling: how the credential crosses the wire

Three options, all needing a keyed hash (so all needing SHA-256, hand-rolled —
the FAT32/ACPI/virtio precedent):

- **Plaintext shared token.** Simplest; the secret crosses in the clear every
  request. A LAN sniffer captures it. Rejected — a design smell this very
  postmortem would have had to apologize for.
- **Server-nonce challenge-response.** Textbook-strong (defeats replay), but
  costs an extra round trip *per connection* — and every remote op opens a fresh
  connection today, so that's an added RTT on every `ls`/`cat`/`write`. More
  state on a one-shot connection.
- **Client-nonce MAC (chosen).** Each request carries `[nonce][mac]` where
  `mac = HMAC(key, nonce ‖ np_body)`. The secret never crosses the wire; it
  **folds into the existing one-shot request** with no extra round trip. It
  gives up only replay-of-observed-ops (a sniffer can replay a captured frame
  verbatim, but cannot forge a *new* path or write) — documented, and fixable
  later with a server nonce or a nonce cache.

**The lesson: shape the security primitive to the transport you already have.**
The one-request-per-connection model (a deliberate "HTTP `Connection: close`"
choice from Phase 1) made the round-trip cost of a challenge-response real and
recurring. The client-nonce MAC is *weaker on paper* but strictly better *in
this system*, because it doesn't fight the connection model. Picking the
textbook-strongest option would have taxed every file operation forever to close
a replay window that a trusted-LAN-first posture explicitly isn't defending yet.

## What fell out for free, and why: the symmetric key

The credential is a **shared cluster secret** — every machine configured with
the same key. That single choice made the hardest-looking case disappear. When B
runs `cpu A`, A spawns a child that imports B's namespace at `/host`; the child's
`/host` reads make A's netd a *client* of B's export (the reverse callback from
Phase 4b). So the connection A→B needs authenticating too — a second direction.
With a **symmetric** key, A already holds the same secret B does, so A signs the
reverse callback with it and B verifies it against its own copy. The bidirectional
`cpu` case needed *zero* extra code. A per-machine-keypair design would have had
to distribute and check public keys in both directions; the symmetric secret
collapses "mutual" into "same." *Choosing the coarsest model that still solves
the problem (cluster membership, not per-peer identity) is what made the
mechanism small.* Per-peer identity and reply-direction auth are the documented
next refinements — not faked, deferred out loud, exactly as trusted-LAN was.

## The bug the foreign observer caught — and the one the "test" didn't

The verification discipline this project keeps preaching — *verify against a
foreign observer, not your own logs* — earned its keep twice in one afternoon,
once by working and once by **failing in an instructive way.**

The auth wire format is shared by the Rust guest (`netd`) and the host-side
Python peers (`np9p_client.py` / `np9p_server.py`). First I sanity-checked the
Python client against the Python server, host-only, no VM: correct key listed the
tree, wrong key was refused. **It passed — and proved nothing.** Both scripts
computed the magic-number constant from a hand-typed hex literal, and I had
transposed two bytes (`AUHT…` for `AUTH…`) in *both*. Two implementations that
share your mistake agree with each other perfectly. The moment I pointed the
Python client at the *real Rust guest*, the correct key was rejected — because
the guest's magic (written correctly, from a grouped hex literal) didn't match
the Python one. **The independent implementation is the one that catches you; a
mirror doesn't.** The fix was to stop hand-typing the constant and derive it from
the ASCII (`int.from_bytes(b"AUTHNP01", "big")`) on the Python side, so the
value could only be wrong if the *intent* was wrong.

This is the same lesson exFAT/ext2 taught against macOS's `fsck`, sharpened: a
foreign observer only observes if it's genuinely foreign. A test harness you
wrote alongside the code, sharing its assumptions, is not a foreign observer —
it's a louder echo.

**The second foreign observer was the compiler.** `FS_ERR_AUTH` needed a slot in
the `FS_ERR_*` sentinel band (`u64::MAX - n`). I picked `MAX - 11` — which was
already `SPAWN_ERR_BAD_ELF`. The auth test didn't catch it (both sides returned
and checked the same numeric value, so the round trip "worked"), but `rustc`'s
`unreachable_patterns` warning did: a later `match` arm on `SPAWN_ERR_BAD_ELF`
became dead code, because an earlier arm on the now-equal `FS_ERR_AUTH` shadowed
it. *A collision that behaves correctly in the feature that introduced it, and
silently corrupts an unrelated one, is exactly what a warning is for.* Moved to
`MAX - 30`, the one free slot left between `FS_ERR_READ_ONLY` and `FS_ERR_MIN`.

## The old traps that came back (they always do)

- **No mutable statics (the `.bss` ceiling).** The cluster key is loaded once at
  boot but is runtime data, so it can't be a `static` — userland's `linker.ld`
  asserts `.data`/`.bss` empty. So it rides `serve`'s stack frame like every
  other piece of `netd`'s state, and had to be **threaded as `&Auth` through the
  entire event loop** — `on_frame` → `handle_tcp` → `handle_conn_segment` →
  `handle_9p`, and `drain_client_messages` → `handle_client` → `handle_rmount`/
  `handle_run` → `tcp_run` → back into `on_frame`. A wide, shallow, mechanical
  thread. The same constraint that shaped the console server and the network
  server shaped this: no globals means state is a parameter, always.

- **Fail-closed by default.** No `\CLUSTER.KEY` on disk ⇒ the export refuses
  every remote client. This is safe for a single-machine run (nobody's mounting
  it) and safe for a misconfigured cluster node (it doesn't silently serve its
  disk to the world). The alternative — "no key means open" — is the kind of
  default that turns a forgotten config into a breach.

## Scope, held

Three things were deliberately *not* built, and saying so is part of the
deliverable:

1. **Reply-direction auth.** The request is authenticated; the reply is not. A
   client trusting the data it reads back is a mutual-auth refinement. The
   request is the capability — gating writes and remote-exec is what matters
   first.
2. **Replay protection.** The client nonce isn't remembered, so a captured frame
   can be replayed verbatim. Forgery of a *new* request is what's prevented. A
   server nonce or a bounded nonce cache is the next tier.
3. **Per-capability tiers**, beyond the one cheap lever that earned its place:
   `\NOEXEC` lets a machine share its disk (authenticated mounts) while refusing
   `NP_RUN` entirely — because remote code execution is a categorically larger
   blast radius than disk sharing, and turning it off is a one-line guard. Finer
   tiers (read-only vs read-write peers) wait for a consumer, per the standing
   rule.

## Verified

- **HMAC-SHA256**, hand-rolled, checked against NIST SHA-256 and RFC 4231
  HMAC-SHA256 known-answer vectors (empty/`abc`/multi-block; RFC cases 1/2/4 plus
  an oversized key) — the crypto's foreign observer.
- **Cross-implementation, against a real guest** (`make run-image-9p` +
  `np9p_client.py`): the Python-signed request, correct key, made the guest's
  Rust `authenticate()` serve its real disk (`readdir /`, `read
  /EFI/ORBS/INIT.CFG`); a wrong key was rejected (`FS_ERR_AUTH`). Zero `EL0`
  faults, no supervisor restarts, in the guest log.
- **Symmetric two-VM** (`run-image-2vm-a`/`-b`, both images derived from the same
  ESP so they share the key): `mount -r` and `cpu` authenticate with matching
  keys; a machine with a mismatched or absent key is refused cleanly.

## The one-line lesson

*The strongest security primitive on paper is the wrong one if it fights the
system you already built; and a test that shares your code's assumptions will
confirm your bug as confidently as your correctness — only a genuinely
independent observer, the compiler included, tells you the truth.*

## Tier-2 addendum — reply authentication (2026-08-26, v0.13.0)

Tier 1 (above) authenticated the *request*. The reply stayed plaintext, which
this addendum closes: the export now MACs its reply too, so the exchange is
**mutually authenticated** — an active injector can no longer feed a client
forged data.

Two things made it small and worth banking now even though the LAN is trusted:

- **The symmetric key made it nearly free.** The server already shares the key,
  so it just applies the same `hmac.rs` to the other direction. And binding the
  reply MAC to the *request nonce* (rather than minting a fresh reply nonce)
  meant **no new wire field** — both sides already hold that nonce — while also
  tying each reply to its specific request. The right primitive reused in the
  right place costs almost nothing; that's the whole reason it was worth doing
  ahead of its threat model as defense-in-depth.

- **The honest boundary is confidentiality.** This is the line worth stating
  loudly: reply-auth adds *integrity and authenticity*, **not secrecy**. Every
  byte still crosses the wire in cleartext. It would be easy to let "the export
  is authenticated both ways now" quietly imply "the export is secure," and it
  is not — a passive sniffer still reads every file. Encryption is a *separate
  axis*, deliberately deferred behind the "leaving a trusted network" trigger
  along with replay protection and per-peer identity. Naming that boundary is
  part of the deliverable; an authentication win that lets someone believe they
  have confidentiality is worse than no win.

Verified the same way tier 1 was — the independent Python observer verifies the
guest's sealed reply (so the Rust `seal_reply` and the Python HMAC must agree),
and a tampered reply is rejected. The lesson from tier 1 held: the foreign
observer, not the mirror, is what proves it.
