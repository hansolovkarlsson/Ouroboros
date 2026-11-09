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
- **Offline guessing, by anyone who can see the wire.** A user's key is a
  function of their password, and every credential is a signature by that key
  over bytes that cross the network in cleartext (the link carries integrity,
  not confidentiality). So anyone who captures **one** credentialed request,
  on any segment it crosses, can test password guesses offline: derive a key
  from each guess and check it against the signature. Nothing about the
  registry's file mode changes that; the public keys are effectively public.
  Today no user secret crosses the wire at all, so **this is an exposure the
  plan adds**, knowingly, and the same one Kerberos has. What a guessed
  password gives an outsider is the user's console login on nodes where the
  password is used, not cluster access (a request also needs an authorized
  machine's key); what it gives a compromised node is the user's cluster
  identity, which is the threat this plan exists to shrink, so **strong
  passwords are part of the model**, and `manual.md` says so in step 9. The
  defences are the slow derivation (Decision 3) and a realm unique to the
  cluster (Decision 2; a precomputed dictionary for one realm serves every
  node in it, and the dev realm is public on purpose). **The cure is the
  trigger-gated encryption tier**, which would carry the credential
  encrypted; it is recorded there as a reason that tier's trigger now matters
  more (step 9). Masking the credential inside keyed sessions alone was
  considered and not taken: one-shot requests (path verbs, `cpu`) have no
  session key, so it would close half the exposure with new mechanism.

  The slow derivation is only worth its login cost if nothing faster sits
  beside it, and today something does: `/etc/shadow` holds one SHA-256 of salt
  and password, a guess per hash, on every node. So `/etc/shadow` moves to the
  same slow function, keeping its random per-account salt (Decision 10).
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

### Step 0's results (2026-09-26)

**The gate reports rows 1 and 2 served on `main`**, against the ext2 image,
with the guest alive and no restarts: `intruder` claiming root read
`/etc/shadow` (174 bytes, beginning `root:`), and claiming `user` read
`/Users/user/PRIVATE.TXT` (0600, owned by uid 1000, staged by the Makefile for
this gate). That is the gap, observed from outside the code rather than read
from it. Four controls run before the rows, one for each way a row could read
right for the wrong reason: the export enforces modes (`user` claiming
`/etc/shadow` is refused `FS_ERR_PERM`), the intruder key is authorized on the
image (a public file is served), an unauthorized key is refused
(`FS_ERR_AUTH`), and the private file really is `user`'s alone (its mode and
owner, seen through `host` claiming root). The gate takes a stage (`0` today,
`1` for root squash) and checks each row against it. Shown failing both ways:
at stage 1 on `main` it fails row 1, since the root claim is still served, and
against the FAT32 image it fails the mode control, since that disk enforces
nothing.

Rows 3 to 5 print as n/a. Rows 3 and 4 need the credential (step 7). **Row 5
needs an account the dev images do not have**: a non-root name with no
registered key. The registry will list `user` only (Decision 6) and a root
claim is decided by the `root` flag, so neither dev account can be that row's
subject. Step 3, which stages the registry, adds a third dev account for it.

**The iteration costs 5.2 µs of wall time on the QEMU guest**, measured by
`/bin/edtest`: 5,167 to 5,216 ns an iteration over three boots (5,292 and
5,711 on two earlier boots of the same code). That is about **1.09 s a
derivation and 2.2 s a login at 210,000 iterations**, and 0.52 s and 1.04 s at
100,000. The estimate above was 15 to 20 µs, three to four times too
pessimistic: it came from a one-shot HMAC timing that included the first
execution's cost.

What the figure is, since it is the input to a permanent constant:

- It is **wall time with the scheduler included**. An iteration is about
  2.7 µs of CPU (5,000 iterations fitted between two ticks in 13.4 ms), and a
  run long enough to cross ticks loses the CPU at each one, which roughly
  doubles it. A derivation crosses hundreds of ticks, so the wall figure is the
  one a login pays. The runs are 25,000 and 50,000 iterations for that reason.
- It is **checked before it is believed**. The timed step must equal
  `hmac_sha512` (shown failing with a wrong inner pad), and twice the
  iterations must take 1.6 to 3.0 times as long (2.5 at first; boots have read
  1.8 to 2.4, and the review of step 0 found the ceiling too close). That check refused the first
  two versions of the measurement. First, a single run carried about 25 ms of
  something that was not iterations; a warm-up did not remove it and the
  fastest of three runs did, so it was transient (the console drawing the line
  just printed is the likely cause, not proven). Second, runs of 5,000 and
  10,000 iterations straddled the tick boundary, so only the shorter one
  fitted between ticks.
- It is **an idle guest's figure**. A busy guest (a `cpu` job, a transfer)
  makes a login slower, not the constant wrong.

**The count: 210,000** (chosen by Hans, 2026-09-26), OWASP's current figure for
PBKDF2-HMAC-SHA-512. At about 2.2 s a login it is inside Decision 3's budget of
about three seconds with a quarter to spare for a busier guest, so the plan has
no gap against OWASP to record. The literal reading of Decision 3, the largest
count inside three seconds, is about 290,000; it buys a third more guessing
cost for a login at the edge of the budget on an idle guest, and was not
taken. Step 5 writes the count into `ninep-abi`.

### Step 1's results (2026-09-26): root squash

**Built.** An `authorized` line takes an optional trailing `root`
(`clusterkeys`, host-tested: a round trip with and without it, and `ROOT`,
`rooted`, `roo`, `root root` and `root extra` each make the line malformed
rather than a peer without root). `netd`'s `map_user` takes the signing peer's
flag as a **required parameter** and refuses a claim resolving to uid 0 or
gid 0 without it (`FS_ERR_AUTH`, signed), with its own `cpu` line ("this host
does not trust yours with root"). The Python export computes the root-equivalent
names from `mkpasswd.py`'s account table and keeps the dev flags as
`DEV_ROOT_PEERS`, which `check-wire-constants.py` holds equal to
`mkclusterkeys.py`'s (shown failing on a dropped flag and on a flag spelt `1`).
`clusterkey peers` shows the flag; `clusterkey line` never writes it, since
trusting a peer with root is the receiving machine's decision. The two
`ninep-abi` comments are corrected.

**The count, all green after the change:** `make test` (the keyed self-test
claims root as `host`), the keyed, path, fid and session gates on their
documented images, `test-async-rmount` (six checks, the guest signing as
`node-a` against the host peer), `test-reentrant-session` (three `cpu` recipes
as root between the nodes), and by hand the two-node `cwrite` as root and the
`/host` callback as root.

**The gate at stage 1:** row 1 refused `FS_ERR_AUTH`, row 2 still served (step
7 closes it), all four controls passing. Beyond the gate, on the guest: a `cpu`
run as `intruder` claiming root gets the squash line while `host` gets
`uid=0(root)` and `intruder` claiming `user` gets `uid=1000(user)`, and a keyed
session opened as `intruder` claiming root is refused while `host`'s keys and
serves. So a keyed session cannot carry root past the check.

**Both controls the step names, measured.** With the refusal mutated out of
`map_user`, the gate's row 1 is served again and it fails. With `host` unflagged
in the Python server, `make test`'s keyed self-test fails with `FS_ERR_AUTH`,
which is the evidence the count found it.

**Not observed: the gid 0 half.** No dev account has primary gid 0 without
uid 0, so a mutation dropping `|| p.gid == 0` would pass every check above. It
is one condition beside the uid test, stated in `map_user`'s doc, and the
Python side computes it from the same account table. Step 3 adds dev accounts
anyway (row 5 needs one); a gid-0 account beside it would let the gate observe
this half.

### Step 2's results (2026-09-26): PBKDF2 in `ed25519/`

`pbkdf2_hmac_sha512(password, salt, iterations: NonZeroU32, out)`, RFC 8018
over the crate's HMAC, sharing its key handling and pad construction with
`HmacSha512` and priming both pads once. An output past RFC 8018's limit
(`(2^32 - 1) * 64` bytes) panics rather than wrapping the block index. Twelve vectors from
`scripts/gen-pbkdf2-vectors.py` (Python's `hashlib`, OpenSSL underneath):
iterations 1, 2, 3, 4,096 and **210,000**, outputs of 32, 64, 65 and 128
bytes, an empty password and salt, a 128-byte password and a 129-byte one that
must be hashed first, and binary bytes. All matched on the first run.

**Controls, measured.** A test asserts every vector sees a flipped password
bit and a flipped salt bit, the 210,000 row included, and every row but that
one an iteration more or fewer (its ±1 is skipped for time only). Against the
code: one extra iteration, the block index counted from 0, and a long password
used without hashing each fail the vector test. `gen-pbkdf2-vectors.py --check`,
run by `make test`, recomputes the crate's table and `edtest`'s two copies with
`hashlib` (shown failing on an edited table row, an edited `edtest` byte, and
`edtest`'s count changed without its vector).

**On the target.** `/bin/edtest` now times the crate's function (step 0's copy
is deleted) and checks two of the vectors on the guest, the 210,000 one
included. PBKDF2's peak stack is 4,480 bytes of 57,344, calibrated like the
others, which is what step 4's `login` adds to the shell.

**The per-iteration figure moves with code placement, not code: 3.9 to 6.2 µs
under TCG.** The first build read 4.2 µs (0.97 s a derivation); a later
`edtest` build read 6.0 to 6.2 µs, steadily, over five boots. An A/B on one
host showed the crate was not the cause (the new crate under the old `edtest`
read 3.9 µs), and the disassembly settled it: `pbkdf2_hmac_sha512` is the same
1,598 instructions in both binaries, byte for byte, at `0x6cac` in one and
`0x6d48` in the other. Only the address differs. The likely mechanism, not
proven, is QEMU's translation blocks, which do not span a 4 KB page, meeting
the hot loop differently. Two consequences:

- **The count still holds at the worst reading.** At 6.2 µs a login's two
  derivations are about 2.6 s, inside Decision 3's budget of about three
  seconds with less to spare than the quarter step 0 estimated. At 3.9 µs they
  are 1.6 s.
- **A TCG timing is a band, not a number, and no single build's reading should
  be quoted as the cost.** Step 0's figures (5.1 to 5.7 µs, a copy of the loop
  at another address) sit inside the same band. The Pi 4 runs natively and
  will settle the real-hardware figure when it can boot the arc.

### Step 3's results (2026-09-26): the user key, the realm, the registry, version-2 shadow

**Built.** `ninep-abi` gains `SIG_DOMAIN_USERKEY` (29 bytes, so
`SIG_DOMAIN_MAX` did not grow), `USERKEY_ITERATIONS` (210,000) and
`REALM_MAX` (32); step 5 writes the normative prose around them. `clusterkeys`
parses the realm, builds the length-prefixed salt, derives a user's seed, and
reads and writes the registry, whose lookup is **fail-closed**: a line that
names the user but cannot be read, or two lines with different keys, is
`Broken`, never `Unregistered`, since an unregistered name is served without a
credential and a typo must not switch attestation off. `accounts` reads,
writes and verifies version-2 shadow lines (`2$` and 14 hex digits of salt,
exactly as wide as version 1's 8-byte salt), and `Secret` carries its version,
so `usermod`, which copies a secret into a new line, now keeps a version-2
marker instead of dropping it. `clusterkey realm` shows the realm and
`clusterkey realm new [-f]` generates one from the entropy device. Every image
stages the dev realm and a registry (0600) listing `user` only, and a third dev
account, `guest` (1001), the name with no registered key that the gate's row 5
will claim.

**The check.** The Rust derivation at the full count agrees with
`mkclusterkeys.py`'s independent Python for `root`, `user` and `guest`: a
host test pins the Python's keys and derives them in Rust, and
`check-wire-constants.py` holds that fixture equal to what the Python derives
now. A version-2 line made by `hashlib` verifies the right password in Rust and
refuses a wrong one, and a version-1 line still verifies.

**The controls, measured.** The salt's fields reordered in the Rust fail the
host test (and the documented-bytes test); reordered in the Python, they fail
`make test`'s agreement check. The length prefixes dropped fail four tests,
among them the one asserting the `ouroboros-dev`/`x` and `ouroboros-de`/`vx`
pair derive different keys. A version-1 line with an 8-byte salt and its
version-2 replacement are the same length, asserted for names of 1, 4 and 32
bytes. On the guest: the registry is refused to `guest` (0600), `realm new`
refuses an existing realm without `-f`, generates one with it, and **refuses
without an entropy device** (booted without `virtio-rng`, the existing realm
left untouched), and only root may run it.

**Local login cannot depend on the realm, by construction rather than by test
here.** `Secret::verify` takes the secret and the password and nothing else,
and the version-2 fixture was made by `hashlib` from the salt and the password
alone. The plan's control (a version-2 line verifies identically with the
realm file present, absent and edited) is a property of `login`, which is the
code that reads the realm; step 4 tests it there.

**A deviation from the plan: no image writes version-2 shadow yet.** The plan
had `mkpasswd.py` write version 2 in this step. But `accountd` still verifies
a user's old password itself on `passwd` (`ACCTOP_PASSWD`), and a version-2
verify is a derivation, about a second on the guest, inside a supervised
server: the thing Decision 10 exists to prevent, and the watchdog is 2.56 s.
Step 4 is where `passwd` computes both hashes and `accountd` only compares. So
`mkpasswd.py` and `useradd` switch to version 2 **in step 4**, together with
the change that makes it safe, and every image stays on version 1 until then.

**Found while here.** The site's manual page had not taken step 1's `[root]`
in its `/etc/cluster` table (step 1 updated the page's prose only and
re-stamped it); fixed in this step. And files `mke2fs -d` stages on the ext2
images are owned by the building host's uid (501 on this machine), not root,
`/etc/cluster/id` included since it was added; root reads them through its
bypass, so nothing fails, but a non-root account with uid 501 would own them.
Not this arc's; recorded here for the roadmap.

**The review of step 3 (ten findings, nine acted on).** Two were real
defects in this step's own work. **A registry that did not fit its reader
failed open**: a user whose line lay past `USERS_MAX` read as unregistered,
served without a credential; `lookup_user` now takes a required `whole`, and a
registry that did not fit is `Broken` for every name. And **`accountd` could
still be made to derive** from any version-2 line, however written, and then
rewrote the line as version 1; it now calls `verify_without_deriving`, which
declines version 2, and refuses. Shown on the guest with a `hashlib` version-2
line staged for `guest`: `guest` logs in (the first version-2 login on the
target, `login` deriving in the shell), `passwd` is refused, the line is
unchanged and `accountd` did not restart; with `accountd` put back on `verify`,
the same boot's `passwd` succeeds and the line comes back as version 1, the
downgrade the fix prevents. The rest: `--random` now deletes a dev realm and
registry left in a reused directory; the realm has one file-size rule every
reader shares (`REALM_FILE_MAX`), and an empty realm file reads as none, so
`realm new -f` can replace it; the dev passwords come from `mkpasswd.py`'s
table rather than a copy; the count is converted once (`USERKEY_COUNT`).
**Deferred to step 4:** `useradd` accepts names up to 64 bytes and a user key
takes at most 32, so such an account can never have one; step 4, which changes
`useradd` anyway, refuses or warns there.

**Still open from step 1:** the gid 0 half of root squash, unobserved for want
of a dev account with primary gid 0. `guest` is not one; it is row 5's subject.

### Step 4's results (2026-09-26): passwords derived where they are typed, keys held by `netd`

**Passwords (4a).** `accountd` never derives. `ACCTOP_SALT` answers a
secret's version and salt (a non-root caller, only their own); `ACCTOP_PASSWD`
now carries the old hash, which `passwd` computes the way the stored version
needs, and the new version-2 salt and hash, so no password crosses and the
server only compares in constant time and writes; `ACCTOP_UPGRADE` (root only)
carries the password and a version-2 line, checks the password against a
version-1 secret with one SHA-256, and writes in place or refuses, never the
whole-file path (`write_secret` takes the length rule as a required
parameter). `login` upgrades a version-1 line at its first login, best effort;
`mkpasswd.py` and `useradd` write version 2; `useradd` refuses a name over 32
bytes. The boot shell gained `TO_ACCT` (every spawnable slot had it), since its
`login` sends the upgrade. On the guest: a version-2 login, `passwd` refused
with a wrong old password and working with the right one, `useradd` creating a
version-2 account that logs in, and a staged version-1 `/etc/shadow` upgraded
at first login **at the same 262 bytes**, both lines verifying after. Controls:
the upgrade sent after dropping to the user is refused and the line stays
version 1; `accountd` padding the new line by one byte is refused and the size
does not move.

**Keys (4b).** `NETOP_KEY_HOLD`/`DROP`/`DROP_MINE`/`LIST` in `netd`
(`held.rs`): only root fills or lists the table, a hold replaces the same
owner's key, a full table (4) refuses and never evicts, a drop is the holder's
or root's, and every seed is wiped (volatile) whichever way it goes. An owner
is alive while its slot still holds the same packed identity and is not a
zombie (`TASK_STATE`, then `TASK_IDENTITY`, so a recycled slot fails the
comparison); no new kernel query was needed. Dead owners are reaped on every
wake of the loop and before a hold or a list. `login` derives the key after the
password is checked, holds it, and wipes its copy; the session loop drops it
at logout and `DROP_MINE` runs before every login prompt. `clusterkey held`
(root) lists the uids; `keyprobe` tries every op as an ordinary user.

**The check and its controls, measured** by `make test-held-keys`, five driven
boots that read the table from the host. All pass. Mutations, each caught:
`netd` accepting a non-root hold (a nested login started by a user then holds
a key, and `keyprobe` fails); `netd` letting anyone drop (`keyprobe` drops the
logged-in user's key); every owner treated as alive (a killed shell keeps its
key); the logout drop AND the prompt's `DROP_MINE` skipped (the key survives
logout). The logout drop skipped ALONE still passes, as the design says: the
prompt's `DROP_MINE` catches it.

**Found, and corrected in the plan's claims.**

- **`MSG_CALL` needs a full-size reply buffer.** Four of this step's calls
  passed 8 to 48 bytes; the kernel validates 768 (`MSG_MAX_LEN`) before
  delivering anything, so `keyprobe` failed every call with `0xffff...` and the
  shell's calls worked only where 768 bytes past the buffer happened to be
  mapped. All four now pass `MSG_MAX_LEN`. The ABI says so; nothing enforces it
  at compile time.
- **A spawned command reaching `netd` must use `ulib::net_call`** (the
  delegation race), which `keyprobe` and `clusterkey held` first did not.
- **One or two derivations inside `accountd` do NOT trip the watchdog; four
  do.** Measured by mutation: two derivations per upgrade, no restart; four,
  `server slot 5 wedged - no progress (runnable)`, and the login survived with
  its note. Decision 10's "a derivation inside any supervised server would trip
  `WEDGE_TICKS`" is too strong; the rule stands for a server whose requests can
  queue (the review of step 3's case), and the restart line is observable, which
  is what the control needed.
- ~~**`netd` wedges intermittently in the held-key rig, blocked on `fsd`.**~~
  **FIXED 2026-09-27 on `main` (#175), and not this step's code.** In
  `make test-held-keys`'s `login-logout` the supervisor declared `netd`
  unresponsive (ping timeout) and restarted it, dropping every held key: 12
  runs in 135 on 2026-09-27. The cause was a false positive, not a deadlock.
  A `cpu` run makes `netd` read the binary from `fsd` as one path-based call
  per 512-byte chunk (98 for `clusterkey`, each re-walking the FAT chain from
  the start, about 4,800 disk reads in all), and a server acks a ping only
  from its main loop. The supervisor samples at ticks and never saw `netd`
  run between calls, so a ping landing with more than 8 ticks of spawn left
  timed out. `cpu_spawn` now sends `netd`'s existing `heartbeat()` after each
  chunk: 0 wedges in 100 runs of this rig with it cherry-picked, against 10
  in 100 without.

  **Two claims this entry made on 2026-09-26 were wrong.** It was not tied to
  logout: one of the twelve came at the check right after login, and "the
  first `cpu` run after logout" was only the check after the longest idle
  stretch. And `fsd` was not idle and answering: the wedge report extended
  with its PC (a temporary line, reverted) showed it `Runnable` in
  `fat32::Fs::find` on a FAT-sector read, working on `netd`'s request, in all
  ten wedges where the live PC was recorded (the other two predate that line:
  one printed no PC, one a saved context that is stale for the running
  task). The 42 quiet runs after the kernel moved were the placement band
  again, not a change of cause.

  With `main` (and #175) merged under this step, the rig ran 5 of 5
  scenarios and `login-logout` 30 of 30 with no wedge, and the step merged
  (#174). The kernel's blocked-on report stays.

**The review of step 4 (ten findings, six acted on, four recorded).** One
was a real defect in this step's own work. **Logout dropped a key by handle,
as root**: the shell restores uid 0 before the drop, root passes
`drop_handle`'s owner check, and a handle is a bare slot number, so after a
`netd` restart a stale handle named whichever login's key had taken that slot
since, and logging out wiped it. Logout now sends `NETOP_KEY_DROP_MINE`, which
`netd` keys on the sender's packed task identity and so can only drop the
shell's own keys; the handle and `Session.key` are gone from the shell. The
rest acted on: `netd` wipes its receive buffer after a `HOLD` (the seed stayed
there, since the buffer is never cleared between messages), `accountd` wipes
its receive buffer after every request, and `login` its request, both of
which held the plaintext password of an upgrade; a first-login upgrade with no
hardware RNG says it used a clock salt, as `passwd` and `useradd` do; and the
"larger than this shell can read" warning names the file, since the realm is
read through it too. `hold_cluster_key`'s "local login never depends on this"
is corrected: nothing there can refuse a login, but it does wait on `netd`,
bounded by the supervisor.
**Recorded, not acted on:** handles carry no generation. Nothing in the tree
names a key by handle now except `keyprobe`'s refusal check, and step 8 finds a
key by uid, not handle; **if a later step comes to name a key by handle, pack a
generation into it first**, as task identities do. And **`accountd` no longer
refuses an empty new password**: `passwd` sends a hash, so the server cannot
tell. The policy now lives in the `passwd` client, and a crafted program can
set its own account's password to empty (or to bytes that lock it); that is
the caller's own account, not an escalation. The login's blocking call, the
per-wake reap's cost and three copies of the version-2 salt code are follow-ups
in `ROADMAP.md`.

**Deferred to step 8:** closing a uid's attested sessions when its last key
goes. There are no attested sessions until step 8, so there is nothing to
close yet; the table is exercised here, as the step says.

### Step 5's results (2026-11-08): the wire, `AUTHNP05`

**What went in.** `ninep-abi`'s normative block gains "User credentials": the
`AUTHNP05` frame, which is `AUTHNP03`'s header at the same offsets, then a
credential block `[kind:2][clen:2][credential]` (little-endian), then the NP
message. Kind 1 is the user's Ed25519 signature over `SIG_DOMAIN_USER ‖
nonce:16 ‖ name:32 ‖ pubkey:32 ‖ export:32 ‖ SHA-512(NP message):64`, as
Decision 8 says since its amendment (the review, below).
The block also states the user key's derivation and the realm rule (1 to 32
bytes, `0x21` to `0x7e`, whitespace trimmed, no realm means no credential),
and the export's order: the machine half first, then the block, then policy.
A block that runs past the frame, a kind the export does not know, or a known
kind with any other length refuses the request; an unknown kind is never
skipped. New constants: `NP_AUTH_MAGIC_USER`, the block's field lengths and
offset, `NP_CRED_KIND_USER_SIG`, `NP_CRED_USER_SIG_LEN`, `NP_CRED_MAX`,
`NP_AUTH_HDR_USER_MAX` (220), `NP_MSG_HASH_LEN`, `NP_USER_SIG_PREFIX_LEN` (176),
and two tags. Three functions for steps 7 and 8 to call, so the Rust side reads
the rules once: `credentialed` takes a frame from its length prefix and
returns the kind, the credential and the NP message as slices, cut to the
declared length (no caller adds an offset or cuts the frame);
`user_sig_input` lays out the user signature's bytes; and `request_sig_domain`
names the machine signature's tag by magic, so no site can sign a credentialed
frame under the old tag.

**One decision the plan left open.** Decision 8 says the credential block is
"covered by the machine signature" but not under which tag. It is **its own,
`SIG_DOMAIN_REQUEST_CRED`**, not `AUTHNP03`'s `SIG_DOMAIN_REQUEST`. Under one
tag, a captured `AUTHNP05` frame reframed as `AUTHNP03`, its credential block
read as the head of the NP message, would carry a machine signature that
still verifies. Today the reframed message's verb would be garbage, but that
is the incidental barrier the request tag's own doc says not to rely on. Both
new tags fit in 29 bytes, so `SIG_DOMAIN_MAX`, and every `netd` buffer sized
from it, is unchanged.

**The review (high, nine findings, all acted on).** Three were contradictions
inside the normative block, each of which an implementation transcribed from
it would have carried: the keyed-session text still said one-shot requests and
`NP_RUN` "stay `AUTHNP03`"; the policy let a registered root through only with
root's credential, against Decision 6's "the flag outranks the registry"; and
it left out Decision 7, so `cpu` with `/host` would have stopped working for
every registered user. The policy is now Decisions 6, 7 and 5 in precedence,
for both formats. One was a hole in Decision 8 itself, the relay above, closed
by binding the export's key. One was an API hazard: the block reader returned
an offset into the slice it was given, which a caller would have used as an
offset into the frame; it now takes the frame and returns the message. The
rest: no helper for the machine signature's tag (now `request_sig_domain`),
`NP_FRAME_MAX`'s doc claiming buffers are sized from it, the Rust side of the
wire check without the fixpoint loop added to the Python side (now one
function for both), and a miscount in a comment this step rewrote.

**What steps 7 and 8 inherit.** `NP_FRAME_MAX` is now sized from the
credentialed header (2320, was 2252). Nothing outside `ninep-abi` uses it.
`netd` sizes its own frames from `NP_AUTH_HDR_SIGNED` (`REMOTE_SBUF`,
`NP_SIGNED_NP_OFF`, and the receive paths), and those grow by
`NP_CRED_HDR_LEN + NP_CRED_MAX` (68 bytes) only where `AUTHNP05` is built or
read, which is the stack measurement Risk 3 names.

**The check and its controls.** `check-wire-constants.py` pins all fifteen new
names: the twelve wire names on both Python peers, and the three derivation
constants (`SIG_DOMAIN_USERKEY`, `USERKEY_ITERATIONS`, `REALM_MAX`, which no
peer spelled before) on the client alone, since the server verifies against a
registered public key and never derives. Both declare them ahead of step 6, as
session-auth's wire step did. 199 constants agree, up from 171. To see them, the script had to learn
three things: a `u16`, a literal with `_` separators (`USERKEY_ITERATIONS` is
`210_000`, so it had been invisible in Rust), and sums and bare aliases
resolved together until neither adds a name (`NP_AUTH_HDR_USER_MAX` sums
`NP_CRED_MAX`, an alias the sum pass ran before, and the script refused it as
"no peer spells it"). The per-peer floors were raised to the counts as run,
which found the server's and `sys.h`'s floors each already one behind on
`main`. Controls, each one constant on one side and each caught by name: the
client's credentialed tag, its magic and its iteration count (`210_001`); the
server's kind and its `NP_CRED_MAX` alias; the Rust kind (`u16`), user tag
and hash length; and a server name misspelled, caught by the floor. In the
Rust tests: a short credential accepted, an unknown kind skipped, and name
and pubkey swapped in the signed input, each failing one test. After the
review, five more, each caught: `AUTHNP05` given the old tag, the export left
out of the signed input, the message slice started at the block, an
`AUTHNP03` frame read as credentialed, and a peer's signed-input length
without the export's key (the check names it, 176 against 144).

**The second review (medium, nine findings, six acted on).** A keyed session
opened under Decision 7's rule would have outlived the run that let it open,
since its keyed frames carry no credential to re-check; the block now binds it
to the run, closed when the run ends. `credentialed` takes the frame from its
length prefix and cuts it to the declared length itself, so a receive buffer
passed whole cannot put stale bytes into the hashed message (a test passes
one). Two restatements of the signed input had missed the export key (the
`SIG_DOMAIN_USER` doc, which now points at `user_sig_input` instead of
listing the bytes, and this section's first paragraph), and so had a claim
that both peers pin all fifteen names, and a size in a comment. **Not acted on,
and why:** Decision 7's rule serves any claim of the run's user from the
executing machine while the run is in flight, not only the run's own `/host`
traffic. That is the decision as planned ("bounded by the run and by the pair
of machines"), and narrowing it, for instance to the connection the run opened,
is a decision for step 7, recorded here so it is made there. The Rust
resolver's fixpoint still has no control, as above. The credential block
copied into both peers is by design: each peer is an independent spelling,
and the check compares each with Rust. The Rust
resolver's fixpoint has no control: no Rust constant needs it today, so
removing the loop changes nothing the check can see.

### Step 6's results (2026-11-08): both Python peers

**The client.** `np9p_client.py --password=<pw>` makes every request
`AUTHNP05`, credentialed for `--user` in `--realm` (the dev realm by default).
It derives the user key from the spec on its own, not through
`mkclusterkeys.py`, since it is the third implementation of the derivation
(Risk 5). The export's key in the signed input is `peer_key()`, the key the
reply is checked against. `--cred-misbehave=<mode>` sends a credential wrong in
exactly one way, so each mode checks that an export binds that one thing:
`wrong-key`, `other-name`, `other-machine`, `other-export`, `other-nonce` and
`other-message` (the plan's four, a credential lifted from another request
being the last two, and the review's export), then `unknown-kind`,
`bad-length`, `old-tag` and `relabel` for the block's and the machine
signature's own rules. Step 8 aims all ten at `netd`.

**The server.** `np9p_server.py` verifies both signed formats' machine half,
each under its own tag, reads `AUTHNP05`'s block by the rules, and puts every
claim through one policy, `admit`, for a single request and a keyed
`NP_SESSION` alike: Decision 6 (the `root` flag outranks the registry,
otherwise only root's own credential) and Decision 5 (a registered name needs a
credential that verifies, an unregistered one is served with its credential
unread, a broken registry line refuses). Not Decision 7: this server never
dispatches an `NP_RUN`, so it never has a run in flight to accept a callback
against. Its registry is the dev cluster's, from a new
`mkclusterkeys.dev_registry()` that the staged `/etc/cluster/users` is now
written from too (byte-identical to before, checked by diffing a staging run),
or `--users=PATH`.

**Who this changes.** The dev registry lists `user`, so an uncredentialed
claim of `user` at this server is now refused, and no guest can make a
credential until step 8. Counted before changing it, as step 1 counted root's:
every rig that drives this server (`test-async-rmount`, `test-call-interrupt`,
`trace-remote-flake`, the `testing-qemu.md` recipes) logs in as root from a
flagged node, which rule 1 serves. The one check that changed is the
self-test's own squash control, "an unflagged peer claiming `user` is
served", which now claims `guest`.

**The check.** `credential_self_test`, in the server's `--self-test` and so in
`make test`, runs every case once over loopback against the client: `user`
served with its credential and refused without; `guest` served without one and
with a wrong one (the credential unread); each of the ten misbehaving modes
refused; root from an unflagged peer refused with its credential while root is
unregistered, then, with a registry that lists root, served with it, refused
without it, and served from a flagged peer without it; `user` refused with a
good credential when its registry line is broken; and a keyed session keyed
and serving when opened with a credential, refused and never keyed without
one. **Every refusal is checked for its reason, not only its status**: the
server records why it refused (`LAST_REFUSAL`), and a mode refused for some
other cause fails. That check caught a bug in the test itself on its first
run: a nested function assigned the reason without `global`, read its own
`None`, and failed. The self-test takes 49 s, up from 24 s.

**The controls**, each one mutation on its own copy of `scripts/`, run in
parallel, each caught and naming its own case: the server accepting a
credential made for any authorized machine (the plan's control: `other-machine`
served) or for any authorized export (`other-export` served); the registry
ignored for an uncredentialed claim; an unknown kind read as kind 1; the
registry outranking the flag; a broken line read as unregistered; one tag for
both formats (`old-tag` served, every honest credential refused); a keyed
session's opener not judged; and on the client, the export left out of the
signed input and the salt's realm and name swapped, each failing every honest
credential, the second against `mkclusterkeys.py`'s independent derivation.

**Found on the way.** The client's new `REALM = DEV_REALM` crashed
`check-wire-constants.py`: its mask resolver ORed a bare alias of a bytes
constant as an integer. A lone alias now takes its value whatever its type.

**The high review** (`/code-review high`, ten findings, nine fixed):

- *The registry was not read by clusterkeys' rules.* The transcription left
  out the `whole` rule (a registry past `USERS_MAX`, 1024 bytes, is broken for
  every name), used Python's whitespace (`\v` and `\f` in, NUL out) where
  clusterkeys uses space, tab, CR, LF and NUL, and let `bytes.fromhex` skip
  whitespace inside a key. The two exports would have judged the same file
  differently, which step 8's comparison would have reported as a credential
  bug. All three rules are now transcribed. The registry is bytes throughout,
  `--users=PATH` is read as `rb`, and `check-wire-constants.py` compares the
  server's `USERS_MAX` with clusterkeys'. The self-test reads seven registry
  shapes directly, each against what clusterkeys answers.
- *"Refused for its own reason" could not tell six modes apart.* Six of the
  modes all fail with "credential does not verify": wrong-key, other-name,
  other-machine, other-export, other-nonce and other-message. Old-tag and
  relabel both failed with a bare "denied". A frame broken in a second way
  would pass. Each mode is now **witnessed**: the refused credential (or, for
  old-tag and relabel, the machine signature) verifies over the honest input
  with only that mode's field changed. `other-nonce` flips every bit of the
  frame's nonce instead of drawing a random one, so the witness can recompute
  it. The unsigned denial now names its cause.
- *The keyed credential cases dropped a server crash.* They now go through
  the same wrapper as the single-request cases, which counts one.
- *`None` meant "as the flags say".* After `--password` or
  `--cred-misbehave`, no caller could ask for an uncredentialed or honest
  frame. The defaults are now `FROM_FLAGS`, and `None` means none.
- *Cleanups:* one header builder for both signed formats, one Ed25519
  reference load in the server, and `dev_registry` derives only the names it
  is asked for.
- *Not fixed:* an `AUTHNP05` frame's block is parsed up to four times per
  request. That is small, and the fix would reshape `serve_connection`.

The controls: the ten above, all still caught, and six new ones, each on its
own copy. The client's `other-export` also changing the nonce, and `old-tag`
signing under the reply tag, are both caught by their witnesses (both frames
are still refused for the expected reason). Folding the denial's causes into
one is caught by its reason. Python's whitespace, the `USERS_MAX` rule
removed, and loose hex are each caught by the registry cases. A server
`USERS_MAX` of 2048 is caught by the checker. The self-test takes 51 s.

**The medium review** (`/code-review medium`, nine findings, eight acted on):

- *`run` ignored `--password`.* `run_op` built its frame with `signed_frame`,
  so an `NP_RUN`, the request Decision 7 depends on, always went out as
  `AUTHNP03`. It now uses `request_frame`, like every other request.
- *The usage examples could not work.* They dialled the host server without
  `--peer=host`, so the credential signed node-a's key as the export. The
  examples were then run against a live server: the honest one is served, and
  `other-export` is refused because its credential does not verify.
- *`Session.open` had no `cred_mode`.* It now has one, as `op` does.
- *`impersonate-gate` ignored `--password`.* Its rows are claims without a
  credential by design, so it now refuses the flag instead of ignoring it.
- *Rule 1 with a credential that is present and wrong.* ninep-abi says a
  flagged peer is served root "without a credential" and says nothing about a
  wrong one being sent. This server serves it without reading it, and a new
  self-test case pins that reading so step 7's netd is held to it. **Whether
  the spec should say so, or refuse instead, is Hans's call.**
- *Cost and fail-open cleanups:*
  - `other_member` is one memoized choice of the "other" machine. The client
    builds with it and the witness checks that exact key.
  - The witness memoizes the user keys.
  - The self-test builds its registries from the one already derived.
  - `dev_registry("user")` now raises. `set()` would have made it an empty
    registry, which serves `user` without a credential.

Two more controls, both caught: `other-machine` and `other-export` built on a
different member than `other_member` names (no witness), and a flagged root
claim reading its credential (the pinned case refused).

### Step 7's results (2026-11-08): the export, in `netd`

**What went in.** `netd`'s export verifies both signed formats, each under its
own tag (`request_sig_domain`), and reads an `AUTHNP05` frame's block with
ninep-abi's `credentialed`, on the frame cut to its declared length. Every
claim, a single request or a keyed session's opener, is decided by one
function, `admit`, by the normative block's rules in order. `map_user` is now
`find_account`, which only looks the name up, and `admit` is its one caller.
That keeps what the required parameter used to guarantee: no inbound path can
resolve a name without the decision. The refusals each say their cause to a
`cpu` caller, and each is a signed `FS_ERR_AUTH`.

**Decision 7's anchor.** `park_run` resolves the address it dials through
`authorized` (as `handle_rmount` does for a reply's key) and records that
line's key in the run's park. A claim of the run's user signed by that key is
served while the park lives, which is until the run ends. A run to an address
no line names has no anchor and lets nothing in. A keyed session opened under
the rule carries the run's tag (its caller and call) and is closed at its
first frame after the run ends, before that frame is verified. The plan said
"when the run ends"; closing at the next frame serves nothing the plan's
wording would not, and needs no hook from the run's end into the export's
connections. **The narrowing the step 5 review left for this step is not
taken:** a `cpu` child's `/host` reads arrive on fresh connections from the
executing machine's `netd`, not on the run's own, so nothing but a token could
tell them from that machine's other claims of the same user, and Decision 7
rejected a token. The rule is as planned: the run, the pair of machines, the
user.

**The registry is read per claim, not held.** It went into `Auth` first, a
`USERS_MAX` buffer beside `authorized`, and the stack measurement below found
that kilobyte on every path. So `registry_lookup` reads `/etc/cluster/users`
whole into a buffer in its own frame, below `admit` alone, and judges it by
clusterkeys' `lookup_user`: an absent file registers nobody; a file that
cannot be read, or that is past `USERS_MAX` (one read short of the buffer, and
a probe at that offset answering 0), is broken for every name. An edit
takes effect at the next claim, as an edit of `/etc/passwd` already does. The
boot still reads it once, only to say its state. `for_each_line`, the
streaming reader `/etc/passwd` uses, was not reused: it skips a line too long
for its buffer, and a skipped line naming a registered user would read as
unregistered, which fails open.

**The stack**, the whole-run low-water mark (the probe of `8429d38f`, painted
at `serve`'s start), headroom in bytes, the same three rigs on each tree:

| rig | `main` | `Auth` holding the registry | read per claim |
|---|---|---|---|
| `test-async-rmount` (the client's park path, the deepest) | 3,088 | 1,936 | 2,976 |
| `test-run-callback` | 3,424 | 2,272 | 3,312 |
| `impersonate-gate 7` (the export, both verifies) | 5,840 | 4,688 | 5,728 |

Every column moves by a constant: the cost is resident state, not depth.
Holding the registry cost 1,152 bytes on every path; read per claim, the step
costs 112, the run tag each `MAX_CONNS` keyed session carries. The export's
second verify and the transient registry buffer reach no deeper than the
export's existing peak. (With the probe in, two of `test-async-rmount`'s six
checks fail on both trees, timing ones; without it the rig passes six of six
on this branch.)

**The checks.**

- `impersonate-gate 7`, on the ext2 image, every row as the table's
  right-hand column: rows 1 to 3 refused `FS_ERR_AUTH` exactly (an
  `FS_ERR_PERM` would mean the claim was served and a mode stopped it), rows 4
  and 5 served. Row 4 is a credential made by `np9p_client.py`, the
  independent implementation, verified by `netd`. The stage also aims each of
  step 6's ten misbehaving credentials at `netd`, every one refused, and pins
  rule 1 both ways: a flagged peer is served root with a wrong credential, and
  an unflagged one is refused with root's own while root is unregistered. Its
  two controls that need a served non-root claim claim `guest`, unregistered,
  at every stage.
- `make test-run-callback`, new: one boot of the ext2 image as the
  dispatcher, the host playing the executing machine at two dev addresses,
  `host` at 10.0.2.2 (flagged) and `intruder` at 10.0.2.20 by a guestfwd (not
  flagged). Fifteen checks: the run's user served to the run's machine during
  the run, one-shot and on a keyed session; another machine refused (the
  machine bound); after the run, the one-shot refused and the session closed,
  its log line naming the run (the time bound); a run as root to the unflagged
  machine, its callback claiming root, refused, and to the flagged one served
  (a run never lifts the squash); and the registry edited from the guest's
  root shell, a broken `user` line refusing `user`'s valid credential while
  `guest` is served, and the file removed serving `user` uncredentialed.
- `fid-gate` and `keyed-gate` claim `guest` for their different-user checks:
  an uncredentialed claim of the registered `user` is now refused before a
  fid's owner or a session's user is consulted, so the checks would have
  failed for a reason that proves nothing. Both pass.
- `make test-async-rmount` passes.

**The controls**, each one mutation of `netd` on the committed tree, rebuilt
and booted, each failing named checks:

- skip the credential check: rows 2 and 3 served, and every signature mode;
- read any kind as kind 1 (`netd` and ninep-abi's reader both): the kind-9
  credential served;
- one tag for both formats: `old-tag` served and row 4 refused;
- the flag reading the credential: the pinned rule-1 case refused;
- rule 2 removed: the callback and the keyed session refused;
- any machine while a run is in flight: `intruder`'s claim served;
- a session outliving its run: served after the run ended;
- rule 2 above the squash: root's callback to the unflagged machine served;
- a broken registry line read as unregistered: `user` served past it.

**Not covered by a control:** the time bound for a one-shot claim. "In flight"
is the park's own slot, freed when the run ends, so no one-line mutation can
"remember" a finished run; the check is there, its failure is not shown. And a
registry that cannot be read (as against absent) is not reachable from a rig.

**What this step changes for the dev cluster until step 8.** The dev registry
lists `user`, and `netd`'s client sends no credential until step 8, so a
`user` logged in on one guest is refused at the other guest's export: the
hand-run two-node `user` recipes in `testing-qemu.md` refuse between the two
steps. No automated rig claims a non-root user at a guest export (counted
before the change). Steps 7 and 8 belong in one release.

**The high review** (`/code-review high`, nine findings, all acted on):

- *A node with no identity dispatched `AUTHNP05`.* The pre-gate that
  refuses a signed request on a node with peers and no `/etc/cluster/id`
  tested `AUTHNP03`'s magic alone. An `AUTHNP05` frame was authenticated,
  admitted and dispatched: on such a node `cpu ... id` ran and printed
  `uid=1001(guest)`, and a write would have landed, before the unsignable
  reply was refused. The gate now asks `request_sig_domain`, which names
  every signed format. **`make test-no-identity`**, new, boots a copy of the
  ext2 image with the identity removed (debugfs) and sends `cpu id` in both
  formats: both denied. With the old gate put back, `AUTHNP05` runs `id`.
- *A `cpu` caller refused for its block got a framed reply.* `np_offset`
  used `credentialed` to find the verb, which refuses exactly the blocks
  `Refused::Credential` is for, so the denial was framed, and a `cpu` caller
  prints that as binary. It peeks past the block by the block's own `clen`
  now. Stage 7 checks both block modes answered as text; with the old
  `np_offset`, both are answered with the binary frame.
- *A registry read that may clear was answered `FS_ERR_AUTH`.* A transient
  failure (no disk yet, `fsd` restarting) is `Refused::RegistryUnreadable`,
  answered `NO_FS` like an unreadable `/etc/passwd`. No rig reaches it.
- *The callback rig could pass with its checks unrun:* a stand-in thread
  that died recorded nothing. A thread's exception is now a failure, and the
  rig requires all 15 checks. Its after-run checks waited a fixed 3 s; they
  follow the guest's prompt now, which comes back only once `netd` has
  answered the run's caller, where the run's park is freed.
- *Smaller:* `read_registry` reads on until 0 or a full buffer, so a short
  read from `fsd` is not "not whole"; an uncredentialed root claim from an
  unflagged peer reads no registry; `park_remote` takes the run's anchor
  instead of `park_run` patching the park afterwards; the boot warning logs
  `USERS_MAX` and says what it means.

**The medium review** (`/code-review medium`, nine findings, seven acted on):

- *A session was bound to a run it did not need.* `admit` asked rule 2
  before the registry, so a session that rule 3 or 4 would have served (an
  unregistered user's, or one opened with a credential that verifies) was
  bound to a run in flight and torn down when the run ended. Rules 3 and 4
  are asked first now; the set of claims served is the same, since rule 2
  serves whatever the registry says, but only a claim nothing but the run
  lets in is bound. The normative block says so. `test-run-callback` opens a
  keyed session as `user` with `user`'s credential during the run, and it
  must still serve after; with rule 2 first, it is closed. (The first cut of
  that check opened the session as `guest`, which no run as `user` can bind,
  so the control passed it; it was the check that was wrong.)
- *A bound session outlived its run until its next frame.* Its fids stayed
  open as the run's user until a frame arrived or the idle reap. `run_ended`
  now RSTs and drops it (releasing its fids) in the loop pass after
  `service_remotes` frees the run's park. The rig checks the guest closes it
  unprompted, before the host sends anything; with the sweep removed, it is
  still open after 5 s.
- *Smaller:* rule 1 asks the flag before reading the registry; `np_offset`
  bounds its peek by the declared length; `test-no-identity` leaves no
  image behind on a failure and says when it needs e2fsprogs; the gate's
  stages 0 and 1 are marked as describing trees before step 7.
- *Not acted on:* two runs as one user to one machine binding a session to
  the wrong one cannot happen, since `park_run` refuses a second run while
  one is in flight (`run_in_flight` says so now). And the per-claim read's
  cost, at least two `fsd` reads a claim on top of the `/etc/passwd` scan, is
  the price of the stack measured above; a cache would have to answer when
  it goes stale, the question `find_account` declines for the same reason.

Every control re-run on the final tree, each caught: the four of the gate
and seven of `test-run-callback` (rule 2 removed, any machine, no close at
all, no sweep, rule 2 first, rule 2 above the squash, a broken line read as
unregistered). The gate, `keyed-gate`, `test-run-callback` (18),
`test-no-identity` and `test-async-rmount` pass.

### Step 8's results (2026-11-08): the client, in `netd`

**What went in.** A request `netd` parks for a uid whose key it holds is
framed `AUTHNP05`: the user's signature over ninep-abi's `user_sig_input`
(`user_sign`, its own frame), with the key of the `authorized` line for the
address dialled as the export's key, the key the reply is checked against.
That covers a one-shot request, a run, a session's opening `NP_SESSION` and
each verb of a session the export does not key; a keyed session's frames
carry no credential, the session's user having been attested once at its
opening. A caller with no key held sends `AUTHNP03` as before, and so does a
run to an address no line names, whose export key is unknown. The machine
signature takes its tag from `request_sig_domain` of the magic it writes, in
both formats, so the framer cannot sign a credentialed frame under the old
tag.

**The uid is captured at the park.** `Parked::uid` is read from `SENDER_ID`
in the handler of the message that asked, beside the name it resolves to, and
the credential is signed with the key held for that uid; `SENDER_ID` is never
read at sign time, when the last message received is whoever's arrived since.

**A session closes when its user's last key goes.** A session whose opening
carried a credential is marked `attested`, and is used only while a live
login holds a key for its uid (`Held::holds_live`): `park_session` closes
one for the uid it parks that fails the test, before its lookup, and
`sign_parked` sends nothing keyed on one (the guards, since the reviews
below). `close_unheld` closes every attested session whose uid fails it:
politely (a FIN once anything already sent is acked), its fids gone with it,
so the next verb from that uid opens a fresh session, uncredentialed, which
the far side judges afresh. A verb parked on it is refused `FS_ERR_AUTH`,
sent or not (since the medium review). It runs after each key op and after
the idle pass's `reap_dead`; the guards are what guarantee a verb never
rides such a session. This is step 4's deferred item, done.

**The send buffers, and what they cost.** The credential block is 68 bytes,
and every remote slot's send buffer grew by it first. The probe of
`8429d38f` (painted at `serve`'s start, applied uncommitted) then read 2,016
bytes of headroom on the async rig's deepest checks, down 960 from step 7's
2,976 and below the 1,936 step 7 rejected for the resident registry. Three
experiments placed it: removing the new reap and close from `NETOP_RMOUNT`
changed nothing; sending no credential at all changed nothing; the send
buffer back at step 7's size gave 2,928. A frame-size diff of the two builds
named the function: `park_session`, 1,824 bytes, then 2,800, by exactly the
new buffer. It assembles a new `SessionConn` on its own frame and copies it
into the slot, and past step 7's size the send buffer was assembled there
too; writing it by `ptr::write`, or through a constructor never inlined, did
not stop the copy (the second made the frame 3,104). So the tables differ: a
session slot keeps step 7's buffer (`SESSION_SBUF`), which holds the block
beside 684 bytes of message, and the largest fid verb any client sends is a
560-byte `NP_PWRITE` (asserted at build time); only the path-verb table
grows. A fid verb too large to frame with its credential is refused
`FS_ERR_AUTH`, the answer every request too large to frame gets. On the
final tree, headroom in bytes:

| rig | step 7 | step 8 |
|---|---|---|
| `test-async-rmount` (the client's park path, the deepest) | 2,976 | 2,704 |
| `test-run-callback` | 3,312 | 3,040 |
| `impersonate-gate 7` (the export) | 5,728 | 5,360 |
| `test-cred-client` (both nodes) | | 2,704 |

**The checks.** `make test-cred-client`, new (`scripts/test-cred-client.py`):
the two-node ext2 pair, A exporting and B the client, B's console driven and
B's link captured, 17 checks.

- Served: `user` logged in on B reads `/Users/user/PRIVATE.TXT` on A (0600,
  `user`'s) by `cat` and through a keyed session (`cremote`), and
  `cpu A cat /host/...` runs on A as `user`, on B's credential, and reads
  B's copy back through `/host` under Decision 7.
- The key locked: after the logout, root's `su user` (uid 1000, no key held)
  is refused at A, by `cat` and by `cremote`.
- The session case: `cfidwait`, a new C program, holds a remote fid across
  the logout; B's capture shows B's FIN on that session 1.5 s after it
  opened, at the logout, no keyed frame on it after, and the su'd request's
  session opening afresh, signed `AUTHNP03`; the held fid's second read is
  refused. The same with a nested login shell (`/bin/sh`, now staged on the
  ext2 image) killed instead of logged out: the FIN 1.9 s after the session
  opened, and the fid dead.
- Under load from another uid: `user`'s reads are served while root, also
  logged in with a key held, floods `netd` with messages (`cfidwait -n`,
  76,000 to 91,000 in 25 s).
- The capture's census counts `AUTHNP05` (as `drive-2vm.py`'s now does).

Against the foreign verifier: a FAT32 guest logged in as `user` reads
`np9p_server.py`'s `HELLO.TXT` by `cat` and through a keyed session
(`cremote: all checks passed`), and after the logout root's `su user` is
refused by both, the server logging "user is registered and the request
carries no credential" for the one-shot and for the keyed session. The ten
misbehaving credentials of step 6 were aimed at `netd`'s export in step 7's
gate, every one refused; on this `netd` the gate and `test-run-callback` (18)
pass with the probe in, and `test-async-rmount` (6 of 6) with it out, as
does `make test`.

**The controls**, each one mutation of `netd` on the committed tree, rebuilt
and run:

- no credential sent: 12 checks fail, every served one among them, and the
  capture holds no `AUTHNP05`;
- no close on the last drop, the lookup's guard kept: neither held session
  FINned at the logout or the kill; uid 1000 is still refused and the held
  fids still dead, by the guard, which is the evidence that it holds alone;
- neither the close nor the guard: uid 1000 served on the old session, both
  held fids alive, neither session FINned (6 fail).

Before the guard, the no-close control's first run left the two fid checks
green: `cfidwait` held the fid 60 s, past the far export's 30 s idle reap, so
the fid died at A whatever B did. The hold is 25 s now.

**Not covered by a control: the uid captured at the park.** The plan's
mutation, `SENDER_ID` read at sign time, passes every check. The park and the
sign happen in one drain of `netd`'s mailbox, so a credential is signed for
another uid only if that uid's message lands between them, and root's flood
never did. What holds the rule is its shape: the sign has no way to ask who
sent last, only the uid the park recorded. The flood check stays, named for
what it shows.

**Measured.** The client's added cost is one sign from a seed (key expansion
and a signature, `user_sign`): 878 µs on the QEMU guest (`edtest`'s "sign
(one-shot)"), before the first byte is sent, so the capture cannot see it.
The export's is a verify (870 µs) and the hash; from B's capture, request to
reply, `user`'s against `guest`'s by `su guest` (no key held, so `AUTHNP03`;
unregistered, so served), the same file and verb, the medians of five runs
put the added cost at 0.7 to 2.5 ms for a one-shot read and 0.7 to 1.9 ms for
a session's opening. The spread is the rig's, one run's medians over 5 to 18
samples; the floor is the verify.

### Step 8's high review (2026-11-08)

Ten findings.

- *Fixed:* `ed25519::sign` expands the seed into the secret scalar and the
  nonce prefix, as good as the key, on frames nothing wipes, so the expansion
  outlived the logout that wiped the seed. `user_sign` now runs the sign and
  then `scrub_stack` from one frame, the second's 6 KB volatile-zeroed array
  lying over the first's chain (about 3.5 KB by frame size); affordable at
  the top of `serve`, where every signature runs.
- *Fixed:* two logins of one user with a password change between them held
  two keys, and the first slot signed, which could be the stale one. The
  newest hold signs (`Held::seed_for`, by hold order).
- *Fixed, and the guard moved:* closing an attested session was wired as
  `close_unheld` calls at three sites, one a reap on every `NETOP_RMOUNT`, so
  a future path that lost a key without the call would leave a session
  usable as the logged-out user. `park_session`'s lookup now refuses it
  itself (`Held::holds_live`, liveness asked per lookup, no wait for the
  reap); `close_unheld` is the prompt close, at two sites, and the per-verb
  reap is gone. The controls above separate the two.
- *Fixed:* an `NP_OPEN` on the wire when its session closed was answered as
  a success, with a fid that died with the session. It is answered as
  refused.
- *Fixed:* `close_politely`, the one copy of the parked tables' close rule
  (four were); the rig's failure detail printed a list; `sign_parked` copied
  the export key to borrow it.
- *Not changed:* a session opened before its user logged in never becomes
  attested. For a registered user such an opening is refused, so no session
  is kept; for an unregistered one the far side serves on the machine's word
  either way; and a session opened under a run is bound to the run (step 7).
- *Asked, not changed:* a `cpu` run whose login is killed while it runs goes
  on, and B's export goes on serving its `/host` reads under Decision 7. The
  plan closes a uid's sessions with its last key and says nothing of runs;
  whether a run should end with them is Hans's to decide.

On the final tree `test-cred-client` passes 17 of 17, every control as above.

### Step 8's medium review (2026-11-09)

Ten findings.

- *Fixed, one predicate:* a killed login's key went on signing new sessions
  until the idle pass reaped it, because `Held::seed_for` did not ask whether
  the login lived, while the lookup's guard did; and `close_unheld` asked a
  third question, any key at all. A key now counts only while its login
  lives: `seed_for` skips a dead one, and `holds_live`, the one test every
  caller asks, is `seed_for(uid).is_some()`. `Held::holds` is gone.
- *Fixed:* the lookup passed over an attested session whose user was not
  held and opened a fresh one beside it; a later login of the same user made
  the old one usable again, first in slot order, so the fresh session's fids
  were sent down it. `park_session` now closes such a session for the
  endpoint and uid it parks, then looks up, asking liveness once.
- *Fixed:* `sign_parked` refuses a keyed verb on an attested session whose
  user is not held, at the send, so no path that loses a key between the
  park and the send has to have closed the session first.
- *Fixed:* a close for a user who has gone left a verb already on the wire
  to finish, but the FIN follows once the request is acked, and the far
  export drops a connection on its peer's FIN with the reply unsent; the
  caller waited out the deadline. Every parked verb is now answered
  `FS_ERR_AUTH` at the close, sent or not. That removes the high review's
  open rewrite, and with it a fid the rewritten open still counted.
- *Not changed:* a session opened before its user logged in never becomes
  attested; as the high review found, for a registered user such an opening
  is refused.
- *Not changed, a limit:* `scrub_stack` zeroes the stack, not the vector
  registers, and its 6 KB is a measured cover, not a checked one (about
  3.5 KB of frames, from `objdump`). A register is overwritten by netd's next
  work, and is saved only into the kernel's `Context` for this task.
- *Asked, not changed:* a fid verb over 684 bytes framed with a credential
  is refused `FS_ERR_AUTH`, and so only while its user is logged in (the
  results above). Nothing sends one; whether the limit stands is Hans's.

The controls on the final tree, each a mutation of netd that the rig must
fail:

| Mutation | Result |
|---|---|
| No credential sent | caught, 12 checks fail |
| `close_unheld` removed, the guards kept | caught by the FIN within an idle period of the kill; the FIN at the logout now also comes from `park_session`'s close, when the `su`'d request parks |
| Every close and the lookup term removed, `sign_parked`'s check kept | uid 1000 still refused after the logout: the send-time check alone holds (only the kill's FIN fails) |
| All three removed | caught, 6 checks fail: uid 1000 reads the file on the old session |
| A dead login's key still signs (`seed_for` without `alive`) | **not caught**: no verb from the uid falls between the kill and the idle pass's reap. Recorded as not covered, beside the uid-at-sign-time mutation |

`test-cred-client` passes 17 of 17, `test-async-rmount` 6 of 6, and `make
test` exits 0. The probe's headroom: async 2,656, run-callback 2,992, gate
5,360, cred-client 2,656 on both nodes, 48 bytes below the high review's
tree on the client paths (the close loop in `park_session`).

### Step 9's results (2026-11-09): docs, the registry line, the release

**One thing in the plan was never built: the registry line.** Decision 9 has
`useradd` and `passwd` print the `/etc/cluster/users` line for the password
they set, and neither did, and no step owned it. Without it only the dev
images, whose registry `mkclusterkeys.py` stages, could use the arc: a real
admin had no way to learn a user's public key, which only the password
gives. `clusterkeys::user_registry_line` derives it from the realm file,
wipes the seed and formats the line; `useradd` prints it once the account
exists, and `passwd` after a change, finding the caller's own name by uid
when none is given. A node with no realm prints nothing. The line is a
second derivation, another second on the guest.

*Check:* `test-held-keys` gains `registry-lines`: the line `useradd` prints
for a new user, and the lines `passwd` prints for root changing `user` and
for `user` changing their own, each equal to the line `mkclusterkeys.py`'s
independent Python derives, and with the realm removed, no line. A host test
pins the helper to the dev registry's `user` line. *Controls:* `passwd`
looking up uid 0 instead of the caller fails the own-name check; `passwd`
deriving in the dev realm when there is no realm file fails the count. Both
caught.

**The docs.** The normative block's scope paragraph, the `NP_NAME_LEN` doc
and a client rule (when to send `AUTHNP05`, and that a session opened with
it ends with the user's key); `manual.md`'s trust section rewritten around
the two rules, its strong-password paragraph and the realm and registry
setup; `architecture.md`'s new key-agent section; the `login`, `passwd`,
`useradd` and `clusterkey` man pages (`login`'s still described inline
SHA-256 secrets and "no `passwd` yet"); `testing-qemu.md` and
`make-targets.md` for the sixth scenario; the cluster roadmap's per-user and
encryption items, the second with the offline-guessing exposure as a reason
its trigger matters more; `comparison.md`, `gap-analysis.md`, `ROADMAP.md`
item 1 and the site's two pages; the arc moved to `roadmap-completed.md`;
and the changelog. The minor version, v0.25.0, is its own PR, as v0.24.0's
was, and publishing it waits on Hans.

### Step 9's high review (2026-11-09)

Nine findings, all taken.

- *The biggest, a design point:* the line came only from setting a password,
  so registering an existing user meant resetting their password (and root
  learning it), and a new realm meant resetting everyone's. The key is a pure
  function of name, realm and password, so **`clusterkey user <name>`** now
  prints the line from a password typed twice, on standard output alone: the
  user runs it and hands the line over. `useradd` and `passwd` still print it
  for the password they set, as Decision 9 says. The password is not checked
  against `/etc/shadow`, which an ordinary user cannot read; a mistyped one
  makes a line the export refuses the user by, until it is replaced, and the
  retype is what catches it.
- *Silence:* after a change, `passwd` printed nothing when it could not make
  the line (`/etc/passwd` or the realm unreadable, a name a line cannot
  carry), though the key had changed. `ulib::user_registry_line` now reports
  what it found, and every case but "no realm" is said, with the command
  that prints the line later.
- *A comment that was not true:* "the seed is wiped" held for one copy; the
  PBKDF2 output and Ed25519's expansion of the seed stayed on the stack. Both
  go now: the output wiped where it is held, and the stack below the
  derivation scrubbed as `netd`'s signature scrubs it.
- *Copies:* the realm path (three), the line's length (built from the peer
  name limit), the name rule and the read-and-derive (two) each have one copy
  now, in `clusterkeys` and `ulib`.
- *Order:* `useradd` derived the key before steps that can fail; it derives
  after the commit.
- *Docs:* the `clusterkey` page and `comparison.md` stated the credential
  rule without the `cpu` run's exception (Decision 7); `passwd`'s examples
  did not match its output.
- *The rig* derived its expected lines when the file loaded, for every
  scenario; it derives them when a check runs.

`registry-lines` gains `clusterkey user`: the same line as `useradd`, and a
mismatched retype refused. *Controls:* `clusterkey user` without the retype
check, a missing realm read as the dev realm, and `passwd` looking up uid 0
for its own name: each caught.

### Step 9's medium review (2026-11-09)

Nine findings; one was a mutation control's edit, applied while the review
read the tree and reverted by the control script.

- *Fixed:* `ulib` had taken `clusterkeys`, and with it `ed25519`, under every
  program for three tools. The line is `clusterkeys::user_registry_line`
  again, over a realm file the caller reads; `ulib::read_file_status` is the
  one realm reader `useradd`, `passwd` and `clusterkey` share, telling an
  absent realm from an unreadable one. The name and the realm are checked
  before anything is derived, so a refused name is told apart from a
  missing realm, and nothing is derived for either.
- *Fixed:* `clusterkey user` took an empty password, which is no account's;
  refused, as `useradd` and `passwd` refuse one.
- *Fixed:* the typed passwords are wiped in all three tools.
- *Fixed:* `passwd` reads `/etc/passwd` with the checked reader.
- *Fixed:* the rig checked a refused `clusterkey user` said so, not that it
  printed no line; it checks both, and the empty password too.
- *Not changed:* with no name, `passwd` finds the account by uid, the first
  line with it. So does `accountd`, whose password it changed, so the line
  is the changed account's. Two names on one uid are one account to both.
- *Not changed:* `login` reads the realm through the shell's own reader, as
  every account file; an unreadable realm there means no key at this login,
  which a later login retries.

On the final tree `registry-lines` passes 10 of 10. *Controls:* a refusal
that falls through to print a line fails both "prints no line" checks; an
empty password allowed fails the empty-password pair. `make test` exits 0.


### Hans's two decisions from step 8 (2026-11-09)

**A `cpu` run ends with its user.** Step 8's high review found that a run
whose login is killed goes on, and the dispatching node goes on serving the
executing machine's `/host` claims of the user under Decision 7 until the run
finishes. The plan closed a user's sessions with their last key and said
nothing of runs. Hans's answer: close the run too. A request sent with a
credential is marked (`Parked::attested`), and `run_counts`, which
`run_in_flight` and `run_live` both ask, counts an attested run only while a
live login holds its user's key. Liveness is asked there, not after the idle
pass, so Decision 7's window closes in the same pass as the key; `held` is
passed down to the export's `admit` for it. The run arm then ends the run:
the caller gets the output so far and `cpu: the run was ended here: its user
is no longer logged in`. A run sent without a credential (no realm, or no key
held) is unaffected, since its user was never attested. **Not stopped: the
child on the executing machine.** A peer's close reaps no `cpu` child there
(`TcpConn`'s `Drop`); it runs on into a closed connection, as it did before
for any peer that closed mid-run, and its `/host` reads are refused.

*Check:* `test-run-callback` gains the killed login: a nested shell logged in
as `user` sends `cpu` to the stand-in; while the stand-in holds the run,
Ctrl+C detaches the nested shell and root kills it. The run carries `user`'s
credential; the stand-in's claim of `user` is served before the kill and
refused after it; and the guest closes the run. It runs before the registry
checks, which end with no registry, where an uncredentialed claim of `user`
is served regardless. `test-run-callback` passes 22 of 22. *Control:* a run
counted in flight whatever its user: the claim after the kill is served and
the run stays open, both checks failing. Getting there took three versions
of the rig: Ctrl+C with a `fg`-handed shell owning the keyboard detaches it
at once, and the boot shell prints no new prompt, so a wait for `# ` never
ended; and whether the same Ctrl+C also cuts the nested shell's `cpu` call
(`cpu: remote run failed`) depends on timing, seen both ways, so the rig
waits for neither.

**The 684-byte limit stands.** A fid verb framed with a credential must fit
`SESSION_SBUF`, which kept step 7's size, so one over 684 bytes is refused
`FS_ERR_AUTH`, and only while its user is logged in. The largest fixed one is
a 560-byte `NP_PWRITE`, asserted at build time. The one whose size a caller
chooses is `NP_OPEN`, by its path, and the export's `fsd` refuses any fid
path over 96 bytes (`FID_PATH_MAX`), so no request the limit refuses would
be served. Lifting it would cost 976 bytes of headroom on the client's
deepest path. Hans's answer: accept it, recorded here.

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
export's public key:32 ‖ SHA-512(NP message):64`, every field fixed-width (the
name NUL-padded, as in `AUTHNP03`). *(The export's key was added at step 5, by
its review: without it, an export that received a credentialed one-shot
request could relay the whole frame to a third machine that authorizes the
caller, and act there as the user. The client already knows the key, by the
address it dialled, to verify the reply.)* This is the one definition: the client's `netd` builds and signs
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
mode 0600 on ext2 (the same keys are recoverable from any captured
credential, above, so this is tidiness, not a defence). Its size is a stack budget like
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
   test; an off-by-one iteration count fails. `/bin/edtest` then times the
   crate's function instead of its own copy of the pad priming (step 0's
   instrument), so the figure the count was chosen from is re-measured on the
   code a login runs, and the copy, which cannot take a key over one block, is
   deleted.
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
   **Also, moved here from step 3:** `mkpasswd.py` and `useradd` write
   version-2 shadow, once `passwd` derives and `accountd` only compares (step
   3's results say why); and `useradd` refuses, or warns on, a name longer
   than a user key can carry (32 bytes, against its own 64).
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
   roadmap's security tier (its encryption item gains the offline-guessing
   exposure as a reason its trigger matters more), `manual.md`'s advice that
   strong passwords are now part of the cluster's model, the changelog, and a
   minor version.

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
