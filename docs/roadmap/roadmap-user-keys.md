# Per-user keys for the cluster: the plan

**`ROADMAP.md`'s frontier item 1.** Per-user cluster *identity* shipped on
2026-08-31, and per-machine keys the same day. This is the tier below it: today
the user a remote request names is **asserted by the machine and attested by
nothing**. This plan makes it attested, by a key derived from the user's own
password, without an auth server, and in a shape an auth server can later be
built on.

Grounded in the code at `6d692f5` (2026-09-26). Written before any code, like
[`roadmap-session-auth.md`](roadmap-session-auth.md), whose structure it
follows: a step 0 that measures, decisions scored on the standing order, and
steps that each name a check and a control that shows the check can fail.

## The gap, stated from the code

A remote request is signed by the calling **machine's** Ed25519 key
(`AUTHNP03`, the normative block in `ninep-abi`). The requesting user travels as
a 32-byte name inside that signature. The client's `netd` fills it in from its
own `/etc/passwd` (`caller_name`, from the caller's `SENDER_ID` uid), and the
export resolves it through *its* `/etc/passwd` (`map_user`) and hands the result
to `fsd` as a required parameter. The export checks that the machine is in
`/etc/cluster/authorized` and that the name exists locally. It cannot check that
the user is really at the other end, and `netd`'s own comment at `map_user`'s
call site says so.

Two consequences, one worse than the roadmap states:

- **Any authorized machine can claim any user who exists on the export.** The
  roadmap words this as "any of its own users"; the code does not check that
  the name exists on the *calling* machine at all.
- **That includes root.** `map_user` has no uid-0 case, so a claimed `root`
  becomes `Proxy{0,0}`, and `fsd`'s root bypass (`mode_allows`,
  `if who.uid == 0`) applies. Every authorized machine is root on every export
  that lists it. And it includes **group root**: a claimed account whose
  primary gid is 0 is authorized on group 0 by the same `mode_allows`, so
  root-group files (0640, 0660) open to it.

The published docs said "any of its own users' names" until this plan
corrected them (`manual.md`, `ROADMAP.md`, `gap-analysis.md`,
`comparison.md` and the site's manual page, in the same PR). Two code comments
say it too, `ninep-abi`'s normative block and `netd` at `map_user`'s call
site; they are corrected in step 1, which edits both places anyway.

The defence today is that every authorized machine is trusted. That is
proportionate for two QEMU VMs and a home network, and it is the whole of the
defence: one compromised node is root everywhere.

## What this plan closes, and what it does not

**Closes:** a compromised node can no longer act as a user who has not logged
in on it. The user's key exists only while that user is logged in on a node,
derived from the password they type, and held by `accountd`. At rest there is
nothing on the node to steal but a public key.

**And, first, root:** a claimed root is refused unless the export explicitly
trusts that peer with root, or (once step 7 lands) root's own credential
verifies.

**Does not close**, stated so nobody has to discover it:

- **A user who logs in on a compromised node is exposed.** The node sees the
  password as it is typed. No design without a trusted terminal can prevent
  that; an auth server does not either.
- **Offline guessing.** A user's public key is a function of their password,
  so whoever holds it can test guesses against it. The key derivation is
  deliberately slow (Decision 3), and the public keys are root-readable only,
  but a weak password stays weak. The slow derivation is only worth its login
  cost if nothing faster sits beside it, and today something does:
  `/etc/shadow` holds one SHA-256 of salt and password, a guess per hash, on
  every node. So `/etc/shadow` moves onto the same derivation (Decision 10);
  until it does, the public keys add no resistance the shadow file does not
  already give away.
- **One-shot requests stay replayable**, as they are today. This plan changes
  who a request can claim to be, not whether it can be sent twice.
- **Onward delegation.** A `cpu` command running on another node holds no
  user key there, so it cannot act as its user towards a third node. That is
  `ROADMAP.md`'s frontier item 4, transitive delegation. The `/host` callback to
  the dispatching node is the one case handled (Decision 7).

## Step 0: the measurements, and the gate that fails today

Two things decide this plan's shape and neither is known yet.

**The key-derivation cost.** Decision 3 derives the key with PBKDF2 over
HMAC-SHA-512, which `ed25519/` already has. One PBKDF2 iteration is two
SHA-512 compressions once the HMAC pads are precomputed. From step 2 of the
session-auth arc (a MAC over a ~2 KB message, about 17 blocks, took 120 to
160 µs under TCG) that is an **estimate** of 15 to 20 µs an iteration on the
QEMU guest, so 100,000 iterations would be 1.5 to 2 s at login. That is an
estimate from a different measurement, and the iteration count is a wire
constant every node must agree on forever (Decision 3), so it is measured, not
assumed: `/bin/edtest` times the HMAC compression pair on the guest, and the
count is chosen from the figure on the slowest platform that logs in. The Pi 4
runs natively and will be faster than TCG; QEMU TCG is the floor, so it decides.

**The impersonation gate, built first.** A new `np9p_client.py impersonate-gate`
aims at an export on the ext2 image (the only image where `fsd` enforces
modes, `testing-qemu.md`), signing with an authorized machine key, and runs:

| check | today | after the arc |
|---|---|---|
| claim `root`, read `/etc/shadow` | **served** | refused |
| claim `user` (exists on the export), read a 0600 file of `user`'s | **served** | refused unless `user`'s credential verifies |
| claim `user` with a credential made from the wrong password | n/a | refused |
| claim `user` with `user`'s valid credential | n/a | served |
| claim a name with no registered key | served | served (today's behaviour, Decision 5) |

On `main` it must report the first two rows **served**: that is the gate's
control, run before anything is built, and it is the evidence for the gap
above. A gate that passes on `main` measures nothing. No mutated `netd` is
needed to impersonate: `np9p_client.py --user <name>` already claims any
name. The gate signs as a **dedicated dev identity**, `intruder`
(`mkclusterkeys.py` label `ouroboros-dev-intruder`, a new line in every dev
image's `authorized`), and never as `host`, `node-a` or `node-b`. That keeps
the attacker apart from the identities the existing rigs use, so step 1 can
flag those for root and leave `intruder` unflagged, and the gate still sees the
squash.

## Decisions

Scored on the project's standing order: **stable, safe, and not blocking
future features.** The third is weighted heavily here, because an auth server
with tickets is the natural next tier and this plan must not have to be undone
to get there.

**Decision 1: password-derived keys, not an auth server, not stored keys.**

- *Stored per-user keypairs* (a random key in `/etc/shadow.d/<name>`) do not
  close the gap: root on a compromised node reads every key. And nodes without
  entropy (Parallels, the Pi) cannot generate one.
- *An auth server* (`ROADMAP.md`'s Plan 9 evaluation) closes it, but needs a
  clock or an extra challenge round trip, a ticket cache in the heapless
  `netd`, a new machine role, and a single point of failure, and it moves
  identity from each node's `/etc/passwd` to the cluster, which is a decision
  about the cluster rather than about authentication.
- *Password-derived keys* close it for every user who is not logged in on the
  compromised node, need no entropy, no clock and no master, and keep the
  cluster peer-symmetric. Their cost is distribution: every export must list
  every user's public key, and a password change must reach every export. That
  is tolerable at two to four nodes, and it is precisely what an auth server
  would later remove, which is why this plan is a foundation for one rather
  than an alternative to it.

**Decision 2: the key is the user's, not the node's.** The key is derived from
the password with a salt of `SIG_DOMAIN_USERKEY ‖ len(realm):1 ‖ realm ‖
len(name):1 ‖ name` (each variable field length-prefixed, so no realm and
name can concatenate to another pair's bytes: without the prefixes realm
`ouroboros-dev` with user `x` and realm `ouroboros-de` with user `vx` would
share a salt), where the
**realm** is a per-cluster string staged at image build
(`/etc/cluster/realm`, dev value `ouroboros-dev`). No node identity is in the
derivation, so the same user with the same password has the same key on every
node, which is what lets any node the user logs in on sign for them, and what
an auth server would need. The realm keeps two clusters with the same user
name and password from sharing a key.

**Decision 3: PBKDF2-HMAC-SHA-512, iteration count fixed on the wire.** The
output's first 32 bytes are the Ed25519 seed. PBKDF2 needs nothing but the
HMAC the tree has; scrypt and Argon2 would need memory `accountd` does not
have and code nobody here has written. The iteration count is a normative
constant in `ninep-abi` (all nodes must derive the same key), checked across
all implementations by `scripts/check-wire-constants.py`, and chosen in step 0
as the largest count that keeps a login on the QEMU guest at about one second.
Raising it later changes every user's key, so it is chosen once, carefully.
(OWASP's current figure for PBKDF2-HMAC-SHA-512 is 210,000; if the guest
cannot afford that, the plan says so and records the gap rather than quietly
picking a smaller number.)

**Decision 4: `accountd` is the agent, and the only holder of a user key.**
Plan 9's `factotum`, in the server that already owns the credential store:

- `ACCTOP_UNLOCK(name, password) -> handle`: `accountd` checks the password
  itself (with Decision 10, by deriving the key and comparing it with the
  stored public key, so a login pays one derivation, not two), and records a
  **login handle**: the uid, the key, and the unlocking task's **packed task
  identity** (slot and generation, the kernel's recycled-slot check from the
  async arc's step 0). `login` calls it; `login` never sees the key.
- `ACCTOP_LOCK(handle)`: ends that login. **Only the task that unlocked it may
  lock it** (matched by packed identity), or root; anyone else is refused, so
  no task can log another user out of the cluster.
- `ACCTOP_USERSIGN(uid, nonce, name, machine pubkey, message digest)`: **Only
  `netd` may ask**, checked by the sending task, not by a uid. `accountd`
  builds the signed input itself, exactly Decision 8's, from these fixed-width
  fields, and refuses unless `name` is the name of the uid's held key. `netd`
  supplies the fields and never the bytes to be signed, so it cannot use the
  agent to sign anything that is not a credential for a user who is logged in.

**Keys are held per login, not per uid.** The same user logged in twice (a
nested shell) has two handles and one key, and the key is wiped when the
**last** handle for its uid is locked. `su` asks for no password, so it unlocks
nothing: a shell that became `user` through `su` has no key, and a remote
request from it goes out uncredentialed, which an export where `user` is
registered refuses. That is the intended reading of `su` here, and step 4's
checks cover it.

**A login that ends without logging out** (a shell that crashes, a task killed)
leaves a handle whose owner is gone. `accountd` drops every such handle at the
start of each operation, checked through the packed identity, so a dead
login's key does not outlive the next thing `accountd` is asked. The table is
fixed-size (`MAX_HELD`, 4 handles); an unlock into a full table of live handles
is **refused, never evicting**, and `login` still succeeds, says the cluster
key is not held, and the user's remote requests go out uncredentialed.

**A session never outlives the key that opened it.** A keyed session attested
by a user credential carries no name after its `NP_SESSION`, so it would
otherwise keep acting as the user after logout, until the idle reap. So when a
uid's last handle is locked or dropped, `accountd` sends `netd` a
`NETOP_USER_LOCKED(uid)`, and `netd` closes (FIN) every client session attested
for that uid before it answers anything else. The export sees the connection
end, and the next request from that uid opens a new session, which needs a
credential.

`netd` holds no user key at any time. The kernel grants `netd` `TO_ACCT` and
`accountd` `TO_NET`, neither of which exists today (`tasks.rs`: `NET_TASK`
holds `TO_FSD | TO_CON | CAP_NET`, `ACCT_TASK` holds `TO_FSD | TO_CON`).

**Decision 5: the credential is required per user, not per node.** A user
**with a registered public key** at an export cannot be claimed there without a
valid credential. A user **without** one is served as today (asserted by the
machine), minus root (Decision 6). So there is no flag day and no global
switch: registering a user's key at an export is what turns the check on for
that user there, and removing the line turns it off. This is the "offered
first, required by policy" shape of the session-auth arc, with the policy being
the registry itself.

**Decision 6: root squash, first and on its own.** A claim that resolves to
**uid 0 or primary gid 0** on the export is refused (`FS_ERR_AUTH`, a signed
refusal as for an unknown user; gid 0 because `mode_allows` grants group-root
access on it, above; supplementary groups do not cross the cluster, so the
primary gid is the only group to check) unless the export's
`authorized` line for that peer carries an explicit `root` flag, or (after step
7) the claimed user has a registered key and the request carries that user's
valid credential.
This is step 1 and ships before any key code, because it closes the worst of
the gap with no new crypto. It applies to every verb, `NP_RUN` included, and on
both `netd` and the Python export.

**Decision 7: the `/host` callback is accepted by the dispatcher's own state,
not by a key.** A `cpu` child's `/host` reads come back to the dispatching
node as requests from the *executing* machine claiming the run's user, and
that machine holds no user key. The dispatcher already knows it has an
`NP_RUN` in flight to that machine for that user (`park_run`). So: an export
accepts a claim of user U from machine A **without** U's credential if and only
if it has an `NP_RUN` in flight to A as U, and only until that run ends. No
token crosses the wire, nothing new is secret, and the delegation is exactly
what the user did by typing `cpu`. It is bounded by the run and by the pair of
machines, so it does not become onward delegation.

**It stands in for a user credential and for nothing else: it never lifts the
root squash.** A `cpu` run as root whose child reads `/host` claims root back
at the dispatcher, and that claim is served only if the dispatcher's
`authorized` line for the executing machine carries the `root` flag (Decision
6), exactly as any other claim of root from that machine. So `cpu` as root with
`/host` works between root-flagged peers and nowhere else, and a run in flight
is never a way around the squash. Step 7 tests both halves.

**Decision 8: the wire carries a typed credential.** A new request format,
`AUTHNP05`, is `AUTHNP03`'s header followed by a credential block
`[kind:2][len:2][credential]`, all covered by the machine signature, then the
NP message. Kind 1 is a user signature: Ed25519 by the user's key over
`SIG_DOMAIN_USER ‖ nonce:16 ‖ name:32 ‖ calling machine's public key:32 ‖
SHA-512(NP message):64`, every field fixed-width (the name NUL-padded, as in
`AUTHNP03`). This is the one definition: `accountd` builds exactly these bytes
from the fields `netd` passes (Decision 4), `netd`'s export and
`np9p_server.py` verify exactly these bytes, and `np9p_client.py` signs them.
Who hashes what is fixed too: the client's `netd` computes the SHA-512 of the
NP message it is about to send and passes the digest; nobody else hashes it. The machine key is inside it, so a credential lifted off
the wire cannot be replayed from another machine, and the nonce and message
hash bind it to this request. **A future ticket is kind 2**, a new kind and
not a new format, which is the whole of "not blocking future features" for
the auth server. An export that does not know a kind refuses the request.

A keyed session carries the credential once, in its `NP_SESSION` (sent as
`AUTHNP05`), and every keyed verb runs as that attested user, since the session
is already bound to its opener's name (`SessionAuth::opener`). One-shot
requests (path verbs, `NP_RUN`) carry it on every request. What that costs is
step 8's measurement.

**Decision 9: the registry.** `/etc/cluster/users`, one line per user:
`<name> <pubkey-hex>`, parsed by `clusterkeys` (a pure crate, host-tested),
mode 0600 on ext2 (the public keys are what offline guessing works from, so
local users do not get to read them). Its size is a stack budget like
`AUTHORIZED_MAX`, measured, not assumed. `useradd` and `passwd` print the
line to add; putting it on the other nodes is by hand, as `authorized` lines
are today. That is the N² cost Decision 1 accepts.

**Decision 10: `/etc/shadow` moves onto the same derivation.** Today a shadow
line is `name:salt:SHA-256(salt ‖ password)`, one fast hash, readable by root
on every node, so an attacker who reaches root anywhere guesses at the speed of
SHA-256 and the slow derivation of Decision 3 buys nothing. A **version-2**
shadow line is `name:2:<pubkey-hex>`: the public half of the user's key under
Decisions 2 and 3. Checking a password is deriving the key and comparing the
public key, so a login pays one derivation, the stored secret is as slow to
attack as the registry, and there is no second, faster copy of the same
password anywhere. A version-1 line still verifies the old way and is rewritten
as version 2 by `accountd` at that user's next successful unlock (`accountd`
is already the shadow file's writer). `useradd`, `passwd` and `mkpasswd.py`
write version 2 only.

The shadow file and the registry stay **two files** even though, for a user
whose password is the same everywhere, they hold the same public key. They
answer different questions: shadow says whether this person can log in here,
the registry says whether a remote machine's claim of this person needs their
credential here (Decision 5). Registering is a deliberate act by the export's
administrator; merging the two would turn attestation on implicitly for every
local account, and would make a local password change silently change what
remote claims need. The duplication is checked, not trusted: `clusterkey`
gains a `users` subcommand that reports every registry line whose name has a
version-2 shadow line with a different key.

### Alternatives rejected

- **The machine key signs a per-user credential.** It is the machine signing
  again: a compromised node signs whatever it likes.
- **A per-user key stored in `~/.clusterkey`.** The record would be under the
  control of the principal it authenticates, the same reason `~/.shadow` was
  rejected (`ROADMAP.md`), and root reads it anyway.
- **Encrypting a random per-user key under the password (secstore-style).**
  Would let a password change keep the public key. Needs a cipher the tree does
  not have, and a random key needs entropy the platforms lack. Revisit with the
  auth server, which removes the redistribution cost this would save.
- **Squashing root to `nobody` instead of refusing.** A request that silently
  runs with fewer rights than it asked for fails later and less clearly than
  one refused at the door.

## Steps

The order follows the session-auth arc's: the cheap fix first, then crypto,
then the local agent, then the wire, then the foreign observers, then the
export, then the client. Each side is tested against something that is not
itself.

0. **The measurements and the impersonation gate** (above), with the
   `intruder` dev identity staged in every dev image's `authorized`. *Check:*
   the gate reports rows 1 and 2 **served** on `main`, which is the arc's
   baseline and the gate's own control; the per-iteration cost is measured on
   the guest and the count is chosen from it.
1. **Root squash** (Decision 6). `clusterkeys` learns the optional `root` flag
   on an `authorized` line (host tests); `mkclusterkeys.py` writes it. The
   Python server has **no `authorized` parser**: `dev_authorized()` is a set of
   public keys built from `DEV_PEER_LABELS`, with no per-peer record to carry a
   flag, so it gains one (a per-label `root` bit, dev peers only, since the
   host server serves the dev cluster alone), rather than an all-or-nothing
   squash that could not express "flag exactly the peers the rigs need".
   `netd`'s export and `np9p_server.py` refuse a claim resolving to uid 0 or
   gid 0 from an unflagged peer. The two code comments that state the old
   claim (`ninep-abi`'s normative block, `netd` at `map_user`) are corrected
   here. **Before changing anything, count every check that drives the cluster
   as root**: `drive-2vm.py`'s root rerun, `test-async-rmount`, the `cpu`
   recipes, the host-driven recipes in `testing-qemu.md` (`np9p_client.py`
   claims root by default), and the keyed self-test inside `make test`, which
   runs the client against the server over loopback as `host` claiming root.
   `host`, `node-a` and `node-b` are flagged, so every one of them is
   unchanged; `intruder` is not. *Check:* gate row 1 refused, every counted
   check green. *Controls:* remove the refusal and row 1 is served again;
   unflag `host` and the self-test fails, which proves the count found it.
2. **PBKDF2-HMAC-SHA-512 in `ed25519/`.** There are no RFC vectors for the
   SHA-512 variant, so the vectors come from Python's `hashlib.pbkdf2_hmac`,
   a foreign implementation, generated by a script checked in beside them.
   *Control:* a flipped bit in a vector's password, salt or output fails its
   test; an off-by-one iteration count fails.
3. **The user key, the registry and version-2 shadow** (Decisions 2, 3, 9,
   10). `clusterkeys` parses and formats `/etc/cluster/users` and derives a
   user key from name, realm and password, with the length-prefixed salt;
   `accounts` parses and formats a version-2 shadow line and still verifies a
   version-1 one. `scripts/mkclusterkeys.py` derives the dev users' keys
   (`root`/`root`, `user`/`user`, realm `ouroboros-dev`) **independently**, not
   through a shared helper, and stages the registry and the realm;
   `mkpasswd.py` writes version-2 shadow. *Check:* the Rust and Python
   derivations agree for every dev user, and a version-2 line verifies the
   right password and refuses a wrong one. *Controls:* reorder the salt in
   one of them and they disagree; drop the length prefixes and the
   `ouroboros-dev`/`x` and `ouroboros-de`/`vx` pair derive the same key, which
   a test asserts they must not.
4. **`accountd` as the agent** (Decisions 4 and 10), and `netd`'s `TO_ACCT`
   and `accountd`'s `TO_NET` in the kernel. `login` unlocks and keeps the
   handle; logout locks it. `accountd` upgrades a version-1 shadow line at the
   next unlock. *Check:* after a login, a probe task standing in for `netd`
   gets a signature over Decision 8's bytes that verifies under the registered
   key. *Controls:* a signature request from any task but `netd` is refused
   (removing the check makes the probe's refusal test fail); a sign request
   naming another user than the held key's is refused; a wrong password holds
   nothing; a lock from a task that did not unlock is refused; two logins of
   one user, one logs out, and the other still gets signatures; the last
   logout wipes, so the next sign request is refused; a login shell killed
   without logging out loses its key at `accountd`'s next operation; a fifth
   unlock with four live handles is refused, not evicting; a shell that became
   `user` by `su` gets no signature.
5. **The wire, in `ninep-abi`** (Decision 8). `AUTHNP05`, the credential
   block, kind 1, the new domain tags, the iteration count and the realm rule,
   all in the normative block; `SIG_DOMAIN_MAX` recomputed. The wire check
   learns every new constant on every peer. *Control:* change one constant in
   one peer and the check fails, named.
6. **Both Python peers.** `np9p_client.py` makes a credential from
   `--password`; `np9p_server.py` verifies one against a users file and applies
   Decisions 5, 6 and 7. Misbehaving modes for step 8 to aim at, on the client
   side of the export: a credential under the wrong key, one for another name,
   one bound to another machine's key, one lifted from another request.
   *Check:* the peer self-test (in `make test`) runs each. *Control:* a server
   that skips the machine key in the credential's input accepts the lifted
   credential.
7. **The export, in `netd`.** Registry loaded like `authorized`; `AUTHNP05`
   verified; Decisions 5, 6 and 7 applied. *Check:* the impersonation gate,
   every row as the table's right-hand column, on the ext2 image. *Controls,
   each a mutation of `netd` that must fail named rows:* skip the credential
   check (rows 2 and 3 served); accept an unknown credential kind (a request
   with kind 9 is served); remove Decision 7's rule entirely (a `cpu` run by a
   registered user loses its `/host` reads, which the rig's `/host` check
   catches); widen it by dropping its bounds, so a registered user is accepted
   uncredentialed from any authorized machine rather than only the one running
   that user's `NP_RUN` (the gate's row 2 is served again). And Decision 7
   against the squash, as checks, not mutations: a `cpu` run as root from an
   unflagged machine gets its `/host` reads refused, from a flagged one
   served. Stack measured at the new peak, against `STACK_PAGES` 14.
8. **The client, in `netd`.** For a caller whose uid has a key held in
   `accountd`, `sign_parked` asks for a user signature and frames `AUTHNP05`;
   a keyed session asks once, at its `NP_SESSION`. A caller with no key held
   sends `AUTHNP03` as today. *Check:* on the two-node ext2 rig, `user` logs
   in on B and reads a `user`-only file on A through a keyed session and a path
   verb; `cpu A` with a `/host` read works (Decision 7). *Controls:* each
   misbehaving mode from step 6 aimed at `netd`'s export; `user` logged out on
   B (key locked) is refused at A. **The session case explicitly:** `user`
   opens a keyed session to A, logs out on B, and a request from uid 1000 on B
   (another shell, or root after `su user`) must not ride the old session: the
   capture shows B's FIN on it at the logout, and the request opens a new
   session, uncredentialed, which A refuses. Without `NETOP_USER_LOCKED` it
   would be served as the attested user, which is the control. The census
   learns `AUTHNP05` **before** the
   measurement. *Measured:* the added cost per one-shot request and per session
   open (an IPC round trip to `accountd` plus a sign and a verify), in absolute
   µs, from the capture as well as a probe.
9. **Docs, rigs and the release.** The normative block, `manual.md`'s cluster
   section and its trust paragraph, `architecture.md` (the agent), the man
   pages for `useradd` and `passwd` (and the version-2 shadow line), `testing-qemu.md`'s recipes, the cluster
   roadmap's security tier, the changelog, and a minor version.

## Risks, named in advance

1. **The iteration count is forever.** Every user's key depends on it. Step 0
   measures before step 5 writes it down, and a later change is a new
   derivation version (a new credential kind), not an edit.
2. **Root squash breaks rigs.** Many recipes log in as root, so their remote
   requests claim root. Step 1 counts them before changing the rule, and flags
   exactly the dev peers they need. A rig that goes red for this reason is the
   squash working, and has to be told apart from a regression.
3. **`netd`'s stack.** The client adds an IPC round trip in `sign_parked` and
   the export a second verify beside the machine signature, and the registry is
   resident like `authorized`. `sign_parked` has about 10 KB of headroom; the
   export's peak is the keyed `NP_SESSION`. Measured in steps 7 and 8, with the
   session-auth arc's lesson that a second caller can un-inline a frame.
4. **`accountd` grows from one op to four,** and becomes a secret holder. It
   is small on purpose. The key table is fixed-size, the sign op signs only
   under its own domain tag (so `netd` cannot turn it into a general signing
   oracle), and a review at high effort is part of step 4.
5. **Three implementations of a key derivation.** Rust, `mkclusterkeys.py` and
   `np9p_client.py` must agree byte for byte, and a mismatch reads as a wrong
   password. The vectors come from a foreign implementation, and step 3's
   control is the independence of the two derivations.
6. **Two servers that now call each other.** `netd` calls `accountd` to sign,
   and `accountd` tells `netd` a key is locked. If either did so with a
   blocking `MSG_CALL` while the other was blocked calling it, both would
   hang. So `NETOP_USER_LOCKED` is a one-way `MSG_SEND` that `accountd` never
   waits on, `netd` handles it from its mailbox pass like any other request,
   and `netd` never calls `accountd` from inside that handler. Step 4 states
   the rule where the two message kinds are defined, and step 8's logout check
   exercises it with a sign request in flight.
7. **Firmware and hardware.** The Pi 4 has no cluster networking yet, so the
   arc is QEMU-only, like the rest of the cluster. Login does run on the Pi, and
   the KDF's cost there is recorded when it can be booted.

## Owed from before, unchanged by this plan

The boot identity on the Pi 4 and on Parallels (checkpoint 9 of
`testing-pi4.md`, A6 of `testing-parallels.md`) is still owed from the
session-auth arc. It does not block this one: nothing here uses the boot
counter or boot entropy.
