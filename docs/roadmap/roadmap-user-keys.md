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
say it too, both in `ninep-abi` (the normative block's trust paragraph and the
`NP_NAME_LEN` doc); step 1 corrects them, since it edits that block anyway.
(`netd`'s comment at `map_user`'s call site already says "ANY name".)

The defence today is that every authorized machine is trusted. That is
proportionate for two QEMU VMs and a home network, and it is the whole of the
defence: one compromised node is root everywhere.

## What this plan closes, and what it does not

**Closes:** a compromised node can no longer act as a user who has not logged
in on it. The user's key exists only while that user is logged in on a node,
derived from the password they type, and held by `netd` (Decision 4). At
rest there is nothing on the node to steal but public keys and slow password
hashes.

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
  every node. So `/etc/shadow` moves to the same slow function, keeping its
  random per-account salt (Decision 10). The cluster key's salt cannot be
  random (it must be the same on every node), so a precomputed dictionary
  for one realm serves every node in it: the realm must be unique per cluster
  (Decision 2), and the dev realm is public on purpose.
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
QEMU guest, so 100,000 iterations would be 1.5 to 2 s for one derivation, and
a login on a node with a realm pays **two** (the shadow check and the cluster
key, Decision 10), so 3 to 4 s. That is an
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
**realm** is a per-cluster string (`/etc/cluster/realm`, at most 32 bytes)
staged on every node of a cluster, the same everywhere. It must be **unique to
the cluster**, because a dictionary precomputed for one realm serves every node
in it: `clusterkey realm new` generates one from `RANDOM` on a node that has
entropy (refusing without it, as `clusterkey new` does), and the administrator
copies it to the others, as `authorized` lines are copied today. The dev value,
`ouroboros-dev`, is public and fine for dev rigs only. A node with **no** realm
file derives no cluster key and sends no credential; local login does not read
the realm to check a password (Decision 10); it reads it only afterwards,
to decide whether to derive a cluster key. So a missing, unreadable or edited
realm can cost remote access and never a login. No node identity is in the
derivation, so the same user with the same password has the same key on every
node, which is what lets any node the user logs in on sign for them, and what
an auth server would need. The realm keeps two clusters with the same user
name and password from sharing a key.

**Decision 3: PBKDF2-HMAC-SHA-512, iteration count fixed on the wire.** The
output's first 32 bytes are the Ed25519 seed. PBKDF2 needs nothing but the
HMAC the tree has; scrypt and Argon2 would need memory the shell does not
have and code nobody here has written. The iteration count is a normative
constant in `ninep-abi` (all nodes must derive the same key), checked across
all implementations by `scripts/check-wire-constants.py`, and chosen in step 0
as the largest count that keeps a login on the QEMU guest, **two** derivations
(Decision 10), within about three seconds.
Raising it later changes every user's key, so it is chosen once, carefully.
(OWASP's current figure for PBKDF2-HMAC-SHA-512 is 210,000; if the guest
cannot afford that, the plan says so and records the gap rather than quietly
picking a smaller number.)

**Decision 4: `login` derives the key, and `netd` holds it.** *(Rewritten
after the plan's second review, which found eight of its ten defects in the
first review's repairs, six of them in an earlier version of this decision
that made `accountd` the key holder. That version needed a lock notice from
`accountd` to `netd`, capabilities in both directions, a blocking sign call
from `netd`'s event loop, and a password oracle open to every task; each was a
new way to fail. This version removes those parts rather than guarding them.)*

- **`login` derives the key.** It already reads the password, so keeping the
  key from it protected nothing. After checking the password against
  `/etc/shadow` as it does today (Decision 10), and while the shell is still
  root, `login` derives the user's key (Decisions 2, 3) if the node has a realm
  (`/etc/cluster/realm`), hands it to `netd`, and wipes its own copy. No realm,
  no key: a node that is not in a cluster logs in exactly as it does today.
- **`NETOP_KEY_HOLD(uid, seed) -> handle`**: `netd` records a **held key**,
  returns its slot as a handle, and stores the uid, the
  seed, and the sender's **packed task identity** (slot and generation, the
  kernel's recycled-slot check from the async arc's step 0). **Accepted only
  from a sender whose `SENDER_ID` is uid 0**, so only a root task (in practice
  `login`, before it drops privileges) can give `netd` a key. Nothing else in
  the system can fill the table, so a full table (`MAX_HELD`, 4) refuses the
  hold, never evicts, and `login` still succeeds, saying the cluster key is not
  held.
- **`NETOP_KEY_DROP(handle)`**: the shell sends it at logout, after restoring
  root, with the handle `login` kept. It wipes exactly that entry, so a second
  login of the same user keeps its own. Accepted from the task that held the
  key (packed identity) or from root; anyone else is refused.
- **The boot shell logs in and out in one task.** Login runs in the boot
  shell, slot 0, which loops login and logout without ever exiting and which
  `KILL` refuses, so its packed identity is the same for every session and no
  liveness check ever fires for it. So **a hold replaces any key the same
  owner already holds**, and the login loop sends `NETOP_KEY_DROP_MINE` (every
  entry the sender holds) **before every login prompt**, not only at logout.
  A logout whose drop was lost therefore cannot carry a key into the next
  session: the next prompt drops it before anyone types a name.
- **A dead login's key goes too.** For every other shell (a nested one started
  with `exec`, which can be killed), `netd` checks every held key's owner for
  liveness in its idle pass, beside the idle reap it already runs, and wipes
  the key of an owner that is gone. So a shell that crashes or is killed loses
  its key within one idle period, not at some later operation. (If the kernel
  has no read-only liveness query on a packed identity, step 4 adds one; the
  recycled-slot check the async arc built answers the same question for a
  send.)
- **A session never outlives the keys that attest it.** When the last held
  key for a uid is dropped or wiped, `netd` closes (FIN) every client session
  it opened attested for that uid, in the same pass, before serving anything
  else. It is the same task holding the keys and the sessions, so there is no
  message to lose, spoof or reorder. The next request from that uid opens a new
  session, which needs a credential.
- **Signing is `netd`'s own.** `sign_parked` signs the credential with the held
  key directly, as it signs with the machine key, so no call leaves `netd`'s
  event loop. The uid a request is signed for is **captured when the request
  is parked and stored in the parked request** beside the name, never read
  from `SENDER_ID` at sign time, because `sign_parked` runs in the service pass
  after other receives have replaced the captured credential (`caller_name`'s
  doc says so).

**What a key held here means, stated plainly.** Keys are held per login and
used per uid: while `user` is logged in on a node, anything running there as
uid 1000 can use the key, a shell that became `user` by `su` included. That is
no more than root on that node can already do, and the threat this plan closes
is a node acting as users who are **not** logged in on it. Two logins of one
user hold the key twice; it stays usable until both are dropped. A `netd`
restart loses every held key and every session with them, consistently: users
log in again to make credentialed requests.

**Why `netd` and not `accountd`.** `netd` is the network-facing server, and
holding user keys there means a code-execution bug in it can read the keys of
users logged in at that moment. But it already reads files as `FirstParty`,
its own credential, which is root's (that is how it reads the 0600
`/etc/cluster/id`), so it can already read `/etc/shadow` and anything else,
and it holds the machine key; a compromise of `netd` is already close to a
compromise of the node, which is the case this plan states it does not cover.
Against that small loss, putting the keys where they are used removes every
cross-server mechanism the earlier version needed. `accountd` keeps one new
job, below, and no key.

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

**The `root` flag outranks the registry for root.** A claim resolving to uid 0
or gid 0 from a root-flagged peer is served **without** a credential even if
root is registered at the export: the flag is the explicit statement "this
machine is trusted with root", and Decision 5's per-user rule applies to every
other claim. So registering root at an export changes nothing for flagged
peers and lets an unflagged one in only with root's credential. The dev
registry lists `user` and not `root`, so every root-driven rig step 1 kept
green stays green through step 7; step 7 tests root's credential against a
registry that does list it.

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
`AUTHNP03`). This is the one definition: the client's `netd` builds and signs
exactly these bytes with the held key (Decision 4), `netd`'s export and
`np9p_server.py` verify exactly these bytes, and `np9p_client.py` builds and
signs them the same way. Each side computes the SHA-512 of the NP message
itself, the signer over what it sends and the verifier over what it received.
The machine key is inside it, so a credential lifted off
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

**Decision 10: `/etc/shadow` moves to the same slow function, with its own
random salt.** Today a shadow line is `name:salt:SHA-256(salt ‖ password)`,
one fast hash, readable by root on every node, so an attacker who reaches root
anywhere guesses at the speed of SHA-256 and the slow derivation of Decision 3
buys nothing. A **version-2** line keeps the version-1 shape and widths
exactly: `name:<salt-field>:<hash-hex>`, where a version-1 salt field is 16
hex digits (8 bytes) and a version-2 one is `2$` and 14 hex digits (a 7-byte
random salt; `$` is never hex, so the two cannot be confused), and the hash is
64 hex digits in both: SHA-256 in version 1, the first 32 bytes of
PBKDF2-HMAC-SHA-512 over the salt with Decision 3's iteration count in
version 2. It is independent of the realm and of the cluster key on purpose:
the password check never reads a cluster file, so no cluster misconfiguration
can lock anyone out, and a random salt keeps the shadow file safe from a
dictionary precomputed for a realm. The cost is that a login on a node with a
realm pays two derivations, which step 0 budgets.

**The same width is what makes the upgrade safe.** `accountd` rewrites a
shadow line in place when the file keeps its length (`changed_span`), and
otherwise falls back to a whole-file truncate-then-write, the window
`source-map.md` warns can leave `/etc/shadow` empty on an `fsd` restart or a
power loss, which locks out every account, root included. A version-1 line
with an 8-byte salt and its version-2 replacement are the same length, so the
upgrade is always the in-place path. A version-1 line of any other width
(`SALT_MAX` allows up to 16 bytes, though nothing here has written one) is
**not** upgraded by `login`; it keeps verifying the old way until its owner
or root sets a new password, which writes version 2 like every other password
set (below).

**No supervised server ever derives a key.** The supervisor declares a server
wedged after `WEDGE_TICKS` (128 ticks, about 2.56 s) of continuous
`Runnable`, and one derivation is about 1.5 s under TCG, two for a password
change, before time-slicing stretches either. So every derivation runs in the
program that has the password (`login`, `passwd`, `useradd`), none of which is
supervised, and `accountd` only compares and writes:

- **Upgrade.** After a successful version-1 login, `login` computes the
  version-2 line itself (fresh salt, one derivation) and sends
  `ACCTOP_UPGRADE(name, password, line)`. It is **root-only**; `accountd`
  checks the password against the version-1 line (one fast SHA-256), checks
  the new line's widths, and writes it in place. It cannot check that the new
  hash matches the password without deriving, and it does not need to: root
  may already set any password.
- **`passwd` as a user.** `passwd` asks `accountd` for the account's current
  secret's **version and salt** (`ACCTOP_SALT(name)`; neither is secret),
  computes the old password's hash the way that version does (one SHA-256 for
  version 1, whatever its salt width, and for a legacy secret still inline in
  `/etc/passwd`, which `accountd` already falls back to today; PBKDF2 for
  version 2), computes the new password's version-2 hash over a fresh salt,
  and sends both.
  `accountd` compares the old hash with the stored one in constant time (fast)
  and writes the new line: in place when the width is unchanged, which is
  every version-1 line with an 8-byte salt and every version-2 line, and
  otherwise through the whole-file path `accountd` already takes today for a
  length change (a legacy inline secret, a non-standard salt width), a risk
  this plan does not add to. Presenting the stored hash proves as much
  as presenting the password would to anyone who cannot read `/etc/shadow`,
  and whoever can read it is root, who may set any password anyway.

`useradd` and `mkpasswd.py` write version 2 only. Salts come from `RANDOM`
where there is one and from today's documented weak fallback where there is
not, which is unchanged by this plan.

The shadow file and the registry stay **two files**, and now hold different
things (a salted hash here, a public key there). They answer different
questions: shadow says whether this person can log in here,
the registry says whether a remote machine's claim of this person needs their
credential here (Decision 5). Registering is a deliberate act by the export's
administrator; merging the two would turn attestation on implicitly for every
local account, and would make a local password change silently change what
remote claims need.

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
   gid 0 from an unflagged peer. The two `ninep-abi` comments that state the
   old claim are corrected here. **Before changing anything, count every check that drives the cluster
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
3. **The user key, the registry, the realm and version-2 shadow** (Decisions
   2, 3, 9, 10). `clusterkeys` parses and formats `/etc/cluster/users` and the
   realm, and derives a user key from name, realm and password, with the
   length-prefixed salt; `clusterkey realm new` generates a realm and refuses
   without entropy. `accounts` parses, formats and verifies a version-2 shadow
   line (random salt, PBKDF2) and still verifies a version-1 one.
   `scripts/mkclusterkeys.py` derives the dev users' keys (`root`/`root`,
   `user`/`user`, realm `ouroboros-dev`) **independently**, not through a
   shared helper, and stages the realm and a registry listing `user` only
   (Decision 6); `mkpasswd.py` writes
   version-2 shadow. *Check:* the Rust and Python derivations agree for every
   dev user, and a version-2 line verifies the right password and refuses a
   wrong one. *Controls:* reorder the salt in one of them and they disagree;
   drop the length prefixes and the `ouroboros-dev`/`x` and `ouroboros-de`/`vx`
   pair derive the same key, which a test asserts they must not; a
   version-2 shadow line verifies identically with the realm file present,
   absent and edited, which a test asserts, so local login cannot depend on it;
   a version-1 line with an 8-byte salt and its version-2 replacement have the
   same length, which a test asserts, since the upgrade's safety rests on it.
4. **Holding keys: `login`, the shell and `netd`** (Decisions 4 and 10).
   `login` checks the password as today (version 1 or 2), computes a
   version-2 line and sends `ACCTOP_UPGRADE` after a version-1 login, derives
   the cluster key when there is a realm, and sends `NETOP_KEY_HOLD` before
   dropping to the user, keeping the handle; the shell sends
   `NETOP_KEY_DROP(handle)` at logout and `NETOP_KEY_DROP_MINE` before every
   login prompt. `passwd` derives both hashes itself
   (`ACCTOP_SALT`, then the old and the new) and `accountd` only compares and
   writes. `netd` keeps the held-key table, checks owners'
   liveness in its idle pass, and closes a uid's attested sessions when its last
   key goes (step 8 has sessions to close; this step exercises the table).
   *Check:* after a login, a probe asking `netd` what it holds (a
   diagnostic-only verb, root-only) sees the uid; after logout it does not.
   *Controls:* a `KEY_HOLD` from a non-root task is refused (removing the check
   lets a user's probe fill the table, which the check then sees); a
   `KEY_DROP` from a task that did not hold the key and is not root is refused;
   two logins of one user, one logs out, and the key is still held; a
   **nested** login shell killed without logging out loses its key within one
   idle period (removing the liveness check keeps it, which the check sees);
   for the **boot** shell, which cannot be killed, a mutation that skips the
   logout drop still leaves nothing held once the next login prompt appears
   (removing the prompt's `DROP_MINE` as well keeps it, which is the control); a fifth hold
   with four live keys is refused, not evicting, and that login still
   succeeds; a node with no realm holds nothing and logs in normally;
   `ACCTOP_UPGRADE` from a non-root task is refused, and a version-1 line reads
   as version 2 after one login **with `/etc/shadow`'s length unchanged** (the
   in-place path; a mutation that pads the new line makes the check see the
   length move); `passwd` with a wrong old password is refused; and
   `accountd` stays below the wedge threshold through a login, an upgrade and
   a password change, which the supervisor's restart line would show (a
   mutation that derives inside `accountd` produces that line, which is the
   control).
5. **The wire, in `ninep-abi`** (Decision 8). `AUTHNP05`, the credential
   block, kind 1, the new domain tags, the iteration count and the realm rule,
   all in the normative block; `SIG_DOMAIN_MAX` recomputed. The wire check
   learns every new constant on every peer. *Control:* change one constant in
   one peer and the check fails, named.
6. **Both Python peers.** `np9p_client.py` makes a credential from
   `--password`; `np9p_server.py` verifies one against a users file and applies
   Decisions 5 and 6. Not Decision 7: the Python server only serves and never
   dispatches an `NP_RUN`, so it has no run in flight to accept a callback
   against. Decision 7 is `netd`'s alone and is tested in steps 7 and 8. Misbehaving modes for step 8 to aim at, on the client
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
   catches); widen it, in two separate mutations, by dropping each bound. The
   gate has no run in flight, so it cannot see either, and these two are
   driven on the two-node rig with the host peer as a third machine: while `B`
   has a `cpu` run in flight to `A` as `user`, the host (`intruder`) claims
   `user` at `B` uncredentialed, which must be refused (**the machine bound**;
   the mutation that accepts any machine while some run is in flight serves
   it); and after that run ends, the host, signing with `A`'s dev key (the dev
   seeds are public, `np9p_client.py --sign`), claims `user` at `B`
   uncredentialed, which must be refused (**the time bound**; the mutation that remembers a
   finished run serves it). And Decision 7
   against the squash, as checks, not mutations: a `cpu` run as root from an
   unflagged machine gets its `/host` reads refused, from a flagged one
   served. Stack measured at the new peak, against `STACK_PAGES` 14.
8. **The client, in `netd`.** Every park captures the caller's uid and stores
   it beside the name. For a parked request whose uid has a held key,
   `sign_parked` signs the credential itself and frames `AUTHNP05`;
   a keyed session asks once, at its `NP_SESSION`. A caller with no key held
   sends `AUTHNP03` as today. *Check:* on the two-node ext2 rig, `user` logs
   in on B and reads a `user`-only file on A through a keyed session and a path
   verb; `cpu A` with a `/host` read works (Decision 7). *Controls:* each
   misbehaving mode from step 6 aimed at `netd`'s export; `user` logged out on
   B (key locked) is refused at A. **The session case explicitly:** `user`
   opens a keyed session to A, logs out on B, and a request from uid 1000 on B
   (another shell, or root after `su user` once `user` has logged out) must
   not ride the old session: the capture shows B's FIN on it at the logout,
   and the request opens a new session, uncredentialed, which A refuses.
   Removing the close on the last drop lets it be served as the attested user,
   which is the control. The same with a nested login shell killed instead of
   logged out: the FIN comes within one idle period. And a request parked for
   uid 1000 while other callers' messages arrive before `sign_parked` runs is
   signed for uid 1000, which a mutation reading `SENDER_ID` at sign time
   fails. The census
   learns `AUTHNP05` **before** the
   measurement. *Measured:* the added cost per one-shot request and per session
   open (a sign on the client and a verify on the export), in absolute
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
3. **`netd`'s stack.** The client adds a second Ed25519 sign in `sign_parked`
   and the export a second verify beside the machine signature. The registry
   is resident like `authorized`, and so is the held-key table (`MAX_HELD` ×
   about 48 bytes on `serve`'s frame, the session-auth arc's resident-state
   lesson again), so both are counted, not assumed. `sign_parked` has about 10 KB of headroom; the
   export's peak is the keyed `NP_SESSION`. Measured in steps 7 and 8, with the
   session-auth arc's lesson that a second caller can un-inline a frame.
4. **`netd` becomes a user-key holder.** A code-execution bug in `netd`
   would expose the keys of the users logged in at that moment. Decision 4
   states why that is accepted (`netd` is already root-equivalent in what it
   can read, and holds the machine key). The table is small and fixed, only a
   root sender can fill it, keys are wiped on drop, on an owner's death and on
   `Drop`, and a review at high effort is part of step 4.
5. **Three implementations of a key derivation.** Rust, `mkclusterkeys.py` and
   `np9p_client.py` must agree byte for byte, and a mismatch reads as a wrong
   password. The vectors come from a foreign implementation, and step 3's
   control is the independence of the two derivations.
6. **Login gets slower, and the slow part must stay out of servers.** Two derivations at a login on a node with a realm
   (Decision 10), so the iteration count trades login time against guessing
   cost. Step 0 measures it, and the count is recorded with the login time it
   costs on the QEMU guest, the slowest platform, so the trade is visible. A
   derivation inside any supervised server would trip `WEDGE_TICKS`
   (Decision 10), so the rule that none derives is checked in step 4, not
   assumed.
7. **Firmware and hardware.** The Pi 4 has no cluster networking yet, so the
   arc is QEMU-only, like the rest of the cluster. Login does run on the Pi, and
   the KDF's cost there is recorded when it can be booted.

## Owed from before, unchanged by this plan

The boot identity on the Pi 4 and on Parallels (checkpoint 9 of
`testing-pi4.md`, A6 of `testing-parallels.md`) is still owed from the
session-auth arc. It does not block this one: nothing here uses the boot
counter or boot entropy.
