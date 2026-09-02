---
name: archive-cluster-build-log
description: ARCHIVE - the day-by-day build log of the Ouroboros cluster arc (Phases 0-4, auth, dial-out/in, reply-auth, cpu streaming, v0.5.0-v0.14.0); superseded by docs/CHANGELOG.md and the cluster postmortems, kept only for archaeology
metadata:
  node_type: memory
  type: reference
---

Historical append-log, split out of [[project-cluster-vision]] on 2026-08-31 so
that memory holds the *durable* vision rather than a running diary. Every entry
below is now recorded properly in the repo - `docs/CHANGELOG.md`,
`docs/roadmap-cluster*.md`, `docs/roadmap-completed.md`, and the cluster
postmortems (`cluster-phase0`, `cluster-distributed`, `cluster-auth`, `dial-out`,
`dial-in`, `cpu-streaming`). Prefer those; this is the raw contemporaneous notes.

**The direction Ouroboros is ultimately aiming at** (Hans's years-long personal
goal, committed 2026-08-24): a **Plan 9-style resource-sharing cluster** —
several Ouroboros machines, each exporting resources as file trees, each
composing a private namespace view of the whole cluster's storage/devices/
services. A machine reads another's disk as if local, uses another's network,
runs a program where the CPU/data lives.

Full phased plan: `docs/roadmap-cluster.md`. Spine:
- **Phase 0** — local namespace + one uniform 9P-ish protocol (unifies fsd/cond/
  netd's three bespoke protocols; consumer = multi-mount; delegation becomes
  `bind`). This is the `docs/research-directions.md` "Plan 9 local half", now the
  foundation of the cluster, not a standalone elegance play. **Detailed design
  written 2026-08-25: `docs/roadmap-cluster-phase0.md`** (on main, commit
  4d50a6e, pushed). Resolved
  forks: (1) fused path-based verbs NOW, 9P fids DEFERRED to Phase 1 (fids only
  pay off over the network; add per-client lifecycle to fsd for zero local gain;
  wire header reserves a `tree`/session field so P1 is an extension not a
  reshape); (2) namespace = per-task KERNEL-stored opaque bytes + mount/bind/get
  syscalls, resolution in ulib — the exact CWD_STAGE/GET_CWD pattern (userland
  can't hold statics: PIE linker asserts .data/.bss empty); (3) capabilities
  UNCHANGED — multi-mount lives inside ONE fsd (`[Option<Mount>; MAX_MOUNTS]`,
  one existing TO_FSD), so no new grant; the one-target DELEGATED_SEND limit is
  the named Phase 1 gap for cross-task/remote binds. New `ninep-abi` crate; bulk
  stays grant/safecopy; staged 0a-0e (each shippable); 3-partition multi-mount
  test disk vs host mounts; Phase 0 done = v0.5.0.
- **Phase 1** — 9P-over-TCP (netd carries the file protocol; export/remote-mount).
- **Phase 2** — two-node disk-sharing cluster (THE milestone answering "is it
  doable?" — yes).
- **Phase 3** — all resources as files, remotely mountable (/dev, /net, /proc).
- **Phase 4** — remote execution (Plan 9 `cpu` model: run there, namespace
  imported).
- **Phase 5** — explicit distributed compute (frontier/research).

**Explicitly OUT of scope (the mirage):** shared memory across machines /
transparent single-system-image / transparently splitting one computation across
CPUs. Network latency ~10,000× RAM makes it unusably slow or non-transparent;
every SSI/DSM attempt found the transparency leaks. The achievable substitute is
**explicit** "ship work to the data/CPU" (Phase 4). If a future session sees
drift toward "make the RAM one pool," that's the drift to catch.

Why doable: the two hardest prerequisites already exist and are hardware-
confirmed — a microkernel with servers-over-IPC (fsd/cond/netd) and a working
TCP stack (netd). Key insight: "remote" is just "the same protocol over TCP
instead of local IPC." Testable with two QEMU VMs on a virtual network before
real hardware. Trust starts explicit-trusted-LAN (auth deferred, loudly
documented); consistency starts single-writer.

Status: overall plan + Phase 0 detailed design written. **Phase 0 step 0a+0b
DONE (2026-08-25, branch `cluster-phase0-ninep`, commit e75a7a1, not yet
merged/pushed):** new `ninep-abi` crate (uniform verb set; `tree` selector at
wire offset 8; NP_BASE=0x100; payload@48); fsd `handle_ninep` dual-speaks
alongside FSOP_*; ulib's fs client re-pointed to the verbs (tree=0) so every
/bin fs command reaches fsd over NP with no /bin source change; byte-identical
QEMU regression, zero aborts. Scope note found in build: the SHELL has its OWN
FSOP_* helpers (separate from ulib), so FSOP_* is NOT yet dead. **Shell fs
helpers migrated to NP too (2026-08-25, branch `cluster-phase0-shell`, commit
6f84c7f, not yet merged/pushed):** all six re-pointed via a shell `np_call`;
added NP_READ_AT (exec loader) + NP_WRITE_FILE (write builtin) to complete the
file-op cover; admin ops (mount/format/…) stay FSOP; byte-identical A/B, zero
aborts. **netd migrated + FSOP_* file-op arms DELETED (2026-08-25, branch
`cluster-phase0-netd`, commit a4b7a13, not yet merged/pushed):** netd's fsd
client now speaks NP; with every client migrated (ulib//bin, shell, netd), fsd's
12 FSOP_* file-op match arms were deleted (~210 lines) — NP is the SOLE file
protocol now. FSOP_* is **admin-only** (mount/format/etc.). Verified: fs batch
byte-correct + fsd NOT restarted (ping still acks) + zero aborts, AND netd
`curl GET /BIG.TXT` = HTTP 200, 65536 bytes byte-identical (stat+stream over NP).
DSPOP_* (console) still remains until step 0e. Cosmetic follow-up: a few FSOP_*
file-op *constants* linger as ulib's mkdir/rmdir/touch/rm translation keys.
**Step 0c DONE (2026-08-25, branch `cluster-phase0-namespace`, commit fce52d1,
not yet merged/pushed) — the arc's FIRST kernel change:** per-task namespace
(kernel NAMESPACES store, CWD-shaped) + syscalls NS_SET=52/GET_NS=53 + a
`bind <new> <old>` shell builtin. resolve_ns (longest component-aligned prefix
-> tree+fs_path) added to BOTH ulib and the shell's fs helpers. KEY DESIGN
CHOICE: child inherits the SPAWNING task's namespace AUTOMATICALLY at spawn
(not staged) + bind uses NS_SET on self - simpler than the planned CWD-style
staging, and makes the shell's own cd/write resolve for free. fsd UNTOUCHED
(every bind is tree 0 = remap within the one mount). Verified: regression
byte-identical (empty ns = identity), zero aborts; `bind /mnt /EFI` -> ls/cat/cd
of /mnt hit /EFI, inherited by spawned /bin.
**Step 0d DONE (2026-08-25, branch `cluster-phase0-multimount`, commit b215960,
not yet merged/pushed) - THE PAYOFF, two filesystems at once:** fsd
Option<Filesystem> -> [Option<Filesystem>; MAX_MOUNTS=4] indexed by the NP `tree`
selector; FSOP_MOUNT_AT=19 + vfs::mount_partition mount a specific partition into
a fresh tree; shell `mount <partition> <path>` binds path->tree (shared ns_add
with bind). NO kernel change. Verified on existing run-image-ext2 disk (ext2 P0
auto-mount tree0 + FAT32 ESP P1): ls / = ext2, mount 1 /mnt/f + ls /mnt/f =
FAT32, both read over NP per tree, zero aborts; single-mount regression
byte-identical. **Step 0e DONE + PHASE 0 COMPLETE (2026-08-25, branch `cluster-phase0-cond`,
commit 33a229e, not yet merged/pushed):** cond decodes NP_WRITE_FILE (inline
text) instead of DSPOP_WRITE; all 7 con_write clients migrated; DSPOP_* deleted.
Console writes still address CON_TASK directly (no ns resolution on the echo hot
path - console-server-postmortem concern); /dev/cons-as-namespace-file = Phase 3.
Byte-identical console A/B, zero aborts. **The three bespoke protocols are now
unified: FSOP_* = fsd admin-only, DSPOP_* gone, NETOP_* = client protocol (stays
until Phase 3 /net-as-files).** Phase 0 = namespace + bind + multi-mount +
uniform verb set, all done. v0.5.0 CUT + published (Phase 0), see [[project-release-process]].

**PHASE 1 STARTED (9P-over-TCP) — branch `cluster-phase1-design`, not yet
merged/pushed:** design doc `docs/roadmap-cluster-phase1.md` (commit 8951d51) +
**step 1a DONE (commit 093c54d):** netd exports fsd over TCP. Reframing (local
grant/safecopy -> length-delimited inline frames; NP_NET_PORT=564, NP_NET_MAX in
ninep-abi). netd got a 2nd inbound listener: parse_tcp_in accepts 564,
TcpConn.local_port threads the source port through build_tcp_srv/send_seg,
first-data dispatches by port (80=HTTP start_response, 564=new handle_9p ->
decode frame -> local fsd via list_dir/stat_size/read_file_chunk -> framed
reply). Read-side verbs only (readdir/read/read_file/read_at); writes later.
Verified with a host python 9P client (scripts/np9p_client.py) over
`make run-image-9p` (hostfwd tcp::5640-:564): readdir / -> BIN/ EFI/, read a file
-> real content; HTTP unregressed; zero aborts.
**PHASE 1 COMPLETE (2026-08-25, branch `cluster-phase1c-remote-mount`, commits
a9e7342 + 0dab130, NOT yet merged/pushed):**
- **1c — remote-mount client (a9e7342):** netd NETOP_RMOUNT(endpoint, NP-req)
  frames a verb onto a TCP round trip (reuses tcp_get) -> reply body; unreachable
  = clean NO_FS. Remote namespace binding: tree sentinel NS_REMOTE_TREE=0xFF,
  target=[ip:4][port:2][root]; resolve_ns now returns Resolved{server,tree,
  endpoint,len} (in BOTH ulib and shell's dup layer); fs helpers route remote ->
  np_remote; bulk reads fall back to INLINE chunks (NP_REMOTE_CHUNK=512, no grant
  crosses a machine). Shell `mount -r <host:port> <path>` builtin. No cap change.
  TWO trace-found bugs: (1) parse_tcp hardwired the peer source port to 80 ->
  dropped the SYN-ACK from 564; (2) reused (src-port,ISN) 4-tuple collided with
  peer TIME_WAIT on back-to-back conns -> intermittent stalls; fixed by
  next_src_port (rotating port + derived ISN, clock-derived since a zero-init
  static needs .bss). Plus a PIE trap: `&hostport[..c]` str range-indexing pulls
  in core fmt tables (R_AARCH64_ABS64) -> byte slices + from_utf8. Foreign
  observer: host python 9P SERVER scripts/np9p_server.py + `make
  run-image-9p-client`.
- **1d — two-node integration (0dab130) - THE MILESTONE:** two Ouroboros VMs on a
  shared L2 QEMU socket link (no SLIRP); B `mount -r 10.0.2.10:564 /mnt/a` reads
  A's disk (ls/cat/nested). Per-guest IP: OUR_IP -> our_ip(), last octet from the
  MAC (NET_MAC); default MAC ...:56 -> .15 (SLIRP runs unchanged), two-VM
  ...:0a/...:0b -> .10/.11; no mutable global (reads MAC each call). Makefile
  run-image-2vm-a (listen, run FIRST) / run-image-2vm-b (connect). Verified vs
  shared-link pcap (B ARPs A, TCP :564, framed NP, rotating ports, clean FIN),
  zero -d int aborts on BOTH VMs.
STAGING NOTE: the export gateway shipped first (labeled 1a), so the design's 1a
(outbound) + 1c landed together; nothing left ahead. **v0.6.0 CUT + published**
(Phase 1, tag live) - see [[project-release-process]].

**PHASE 2 COMPLETE (2026-08-25, branch `cluster-phase2-readwrite`, commits
9489935 design + c7c60a3 impl, NOT yet merged/pushed) - THE SHARED-DISK
MILESTONE:** machine B now WRITES machine A's disk over 9P/TCP (mkdir/write/cp/
touch/rm/mv on a remote mount), A sees it on its own disk. Small phase - transport
existed. (a) export gateway handle_9p got the mutate verbs relayed to local fsd:
path-only + mv via fsd_call; NP_WRITE -> fsd inline NP_WRITE_FILE (no grant);
NP_WRITE_AT bridges wire-inline -> fsd grant-based offset write via new
fsd_write_at (mirror of read_file_chunk). (b) client (ulib+shell) fs_write_at/
fs_write_bulk CHUNK a large remote write into <=NP_REMOTE_CHUNK(512) NP_WRITE_AT
round trips, so cp/writeat/>>/write unchanged. (c) tcp_get SYN RETRANSMIT (4
tries) - a single SYN with no retransmit failed the op if the first packet dropped
on a fresh socket link (intermittent first-ls on two-VM); helps fetch+reads too.
(d) semantics: single-writer + clean-disconnect, DOCUMENTED not coordinated (fsd
serializes disk access, so one writer never tears; killed peer -> next op errors
cleanly, no hang/corruption). Verified two VMs: B writes A, A reads own disk sees
it; BYTE-EXACT via foreign observer (macOS mounts A's img, cp'd LSCOPY cmp-identical
to /BIN/LS, 17424 bytes); SIGKILL A -> B errors cleanly + stays responsive; zero
-d int aborts both nodes. **v0.7.0 CUT + published** (Phase 2).

**PHASE 3 STARTED (all resources as files, remotely mountable). Step 1 = /proc
DONE (2026-08-25, branch `cluster-phase3-proc`, commits 71ee259 design + c86ddcd
impl, NOT yet merged/pushed):** the process table as (remote) files. `ls
/mnt/a/proc` on B lists A's live tasks; `cat /mnt/a/proc/2/state` = A's fsd state.
(a) fsd proc.rs = a 4th Filesystem enum arm, the FIRST NON-DISK one (makes the enum
a real VFS) - no storage, generated from the ungated TASK_STATE syscall (`/`->dir
per slot, `/<n>/state`->runnable/blocked/zombie/unused), read-only. Auto-mounted at
boot at reserved ninep-abi NS_PROC_TREE=4 (MAX_MOUNTS->5). (b) shell `mount -p
<path>` binds /proc locally (like mount <n>). (c) netd export PREFIX-ROUTES /proc:
route_export sends /proc-prefixed wire paths to NS_PROC_TREE (prefix stripped) else
disk tree 0; `tree` threaded through fsd_call/read_file_chunk/stat_size/list_dir
(HTTP passes 0). A SCOPED prefix hack, NOT a full namespace-aware export (deferred
until a 2nd synthetic tree needs it). Verified local + two-VM, zero aborts. LIMIT:
only per-slot state (no cross-task argv/name without a new kernel accessor).
**Step 2 = /dev/cons DONE (2026-08-25, same branch `cluster-phase3-proc`, commit
c5eac21):** the console as a WRITABLE file, and the FIRST route to a NON-fsd server
(cond/CON_TASK). `write /mnt/a/dev/cons ...` prints on machine A's screen (remote
console). HOW: ninep-abi NS_CON_TREE sentinel (0xFE, like NS_REMOTE_TREE); shell+ulib
resolve_ns maps it -> server=CON_TASK, np_dispatch errors (write-only), the fs
write helpers con_write the bytes instead of an fsd call; shell `mount -c /dev/cons`
binds it; netd export route_export recognizes /dev/cons and handle_9p emits the
write bytes to CON_TASK (netd already logs to the console). Verified local + two-VM
(B's `write /mnt/a/dev/cons >>>HELLO-ON-A-FROM-B<<<` printed on A), zero aborts.
NOTE: /dev/cons is now the SECOND non-disk prefix special-case - the signal that a
fully namespace-aware export (resolve incoming paths through a composed per-export
namespace, retiring the /proc + /dev/cons prefix hacks) is getting closer to worth
building; still deferred.
**Step 3 = /net DONE + PHASE 3 COMPLETE (2026-08-25, same branch, commit a391cf8):**
/net = the machine's network identity as read-only files (/net/ip dotted-quad,
/net/mac colon-hex), served by NETD ITSELF (first non-fsd, non-cond server fs).
`cat /mnt/a/net/ip` reads machine A's addr. HOW: ninep-abi NS_NET_TREE (0xFD); a
shared net_op serves read verbs from our_ip()/NET_MAC, driven from BOTH the export
(route_export) AND netd's local client handler (now answers NP read verbs addressed
to NET_TASK). Client wrinkle: a /net binding + a remote mount BOTH resolve to
server=NET_TASK -> told apart by the ENDPOINT (local /net=zero, remote=real);
is_local_net routes local reads to np_netlocal (direct NP to NET_TASK, not the
NETOP_RMOUNT wrap); writes refused. shell `mount -n /net`. Verified local + two-VM,
zero aborts.
**PHASE 3 COMPLETE: /proc + /dev/cons + /net, each a file server, each remotely
readable.** Cut as **v0.8.0** (2026-08-25).

**NAMESPACE-AWARE EXPORT DONE (2026-08-26, branch `cluster-ns-aware-export`, commit
8c7f632, NOT merged/pushed - NOT yet released):** the deferred structural cleanup,
justified by the 3rd consumer. The export now serves its OWN composed namespace via
the same resolver a local client uses (Plan 9 "a server exports a namespace"),
retiring the 3 route_export prefix hacks. HOW: (a) the resolver moved to ninep-abi
as ONE `resolve_ns` returning a task-neutral `NsTarget` (Fsd(tree)/Console/NetLocal/
Remote) - no syscall-abi dep; (b) ulib + shell DELETED their duplicate resolve_ns,
now delegate + map NsTarget->server/tree/endpoint (fs helpers untouched); (c) netd
DELETED route_export - handle_9p resolves against a small EXPORT_NS binding blob
(/proc,/dev/cons,/net; else disk) + dispatches on NsTarget. A 4th exported resource
= a 4th binding not a branch. Remote-in-export-ns = transitive re-export, refused
today (a natural Phase 4 hook). NO behavior change; 3 impls -> 1. Verified full
local+two-VM matrix, zero aborts. (Debug tip that paid off: verified resolve_ns in
ISOLATION via throwaway rustc when a remote /net read flaked - resolver was correct,
flake = one-connection-per-op churn, pre-existing.)
**PHASE 4 (remote exec / Plan 9 cpu model) DESIGN WRITTEN (2026-08-26, branch
`cluster-phase4-cpu` off the ns-aware-export branch, commit with
docs/roadmap-cluster-phase4.md; NOT built yet):** end state = `cpu B <cmd>` runs
<cmd> on B with its namespace imported from A (B's CPU, A's files). Staged: 4a =
remote spawn + output STREAM back (no import; runs on B with B's resources), 4b =
namespace import (child on B remote-mounts A via the NsTarget::Remote export hook).
KEY DECISIONS: NETD is the spawner (owns TCP, is an fsd client, its event loop
already drains the mailbox so a child's output messages are captured NON-BLOCKING -
critical since netd is supervised/health-pinged and CAN'T block like the shell's
capture_program_output does); run request = a framed RUN verb on the export conn
whose reply is a STREAM; output CAPTURED+streamed back NOT routed via remote
/dev/cons (con_write bypasses namespace - console-server-postmortem); trusted-LAN,
child reaped/killable/cap-scoped. HONEST ASSESSMENT recorded: 4a is the LARGEST netd
addition since the network stack - child-msg routing by sender slot, a streaming
conn mode (or output-buffering into c.prefix + pump_send), the child->NET_TASK
output-pipe CAPABILITY delegation (unresolved: can netd DELEGATE child->itself?),
reap + flow-control. A deliberate build, flagged before diving into delicate netd.
Design doc has the full plan.
**STEP 4a = remote spawn + output stream DONE (2026-08-26, branch `cluster-phase4-cpu`,
commit 64dc1f1):** `cpu <host:port> <command>` runs <command> on another machine,
output streams back. PROVEN remote by an A-only marker: B's `cpu 10.0.2.10:564 ls /`
-> RANHERE/ (dir only on A). HOW: ninep-abi NP_RUN verb + syscall-abi NETOP_RUN;
netd is the spawner (cpu_spawn: read /bin ELF in 512-BYTE MAX_USER_LEN chunks +
SPAWN_STAGE, stage argv, SPAWN stdout_target=NET_TASK, DELEGATE reply cap); capture
NON-BLOCKING via the event loop (drain_client_messages routes a msg from the child's
slot to its conn's prefix; end-of-stream WAIT-reaps + releases pump_send). CAPABILITY
FIX: NET_TASK holds a self-send TO_NET bit (kernel caps_for_slot) SOLELY to DELEGATE
the reply cap to a child it spawns. Shell `cpu` builtin -> NETOP_RUN -> handle_run
frames NP_RUN, tcp_get round trip, returns streamed output. SERVE LOOP restructured
to iterate conns BY INDEX so the per-segment drain can take &mut conns. SCOPE: runs
with the REMOTE's namespace (cpu B ls / = B's disk); output bounded 1 msg; 4b =
import CALLER's namespace (ns-aware export's NsTarget::Remote hook is ready). Bug hit:
MAX_USER_LEN=512 (staged 2048 first -> "stage fail", found via a netd console
diagnostic). Verified two-VM + full export regression, zero aborts.
**4b (namespace import) ATTEMPTED, BLOCKED on a caller-side DEADLOCK (2026-08-26,
finding committed a6abf93, code reverted):** plan = A's cpu frame carries A's
endpoint; B binds /host->remote(A) on the child (NS_SET, inherited at spawn); child's
/host/... resolves to A. Two of three pieces are easy + were built then reverted:
the /host bind + endpoint, and the child-message DEMUX on B (child talks to netd for
TWO things - stdout MSG_SEND + remote-fs NETOP_RMOUNT MSG_CALL - told apart SAFELY by
op field: a fs call always carries NETOP_RMOUNT op=4 so is never mistaken for output,
which would deadlock the child). THE BLOCKER: `cpu B cat /host/F` needs B's child to
read A's file WHILE A runs cpu, but A's netd services the run with a SYNCHRONOUS
tcp_get (handle_run) that blocks its whole event loop - so A's export can't serve the
child's /host callbacks (tcp_get even receives+DROPS their frames, port-filtered),
child blocks, no output, timeout. Mutual netd dependency the blocking run can't
satisfy. FIX (4b's real work): make handle_run NON-BLOCKING (a netd event-loop state
machine like the server conns) so A drives the run response AND serves the child's
callbacks in one loop. Deferred rather than half-built into deadlock. The /host bind +
demux are ready; the caller-side rework is the gate. Same "netd must never block"
lesson that shaped 4a's capture, now on the CALLER.
**4b = NAMESPACE IMPORT DONE (2026-08-26, branch `cluster-phase4-cpu`, commit
4def4b0) - PHASE 4 COMPLETE, the FULL Plan 9 cpu model:** a remote command runs on
the remote's CPU but reads the CALLER's files via the caller's namespace imported
at /host. `cpu 10.0.2.10:564 cat /host/BONLY.TXT` runs cat on A, prints a file only
on B (caller); `ls /`=A's disk, `ls /host`=B's. HOW: (a) cpu frame carries caller
endpoint (NP_RUN a1/a2); remote binds /host->remote(caller) on ITS OWN namespace
(set_host_ns/NS_SET) before SPAWN, child inherits. (b) DEMUX: child talks to netd
for TWO things - stdout MSG_SEND + /host fs NETOP_RMOUNT MSG_CALL - told apart by op
field (fs call always NETOP_RMOUNT, never mistaken for output=deadlock). (c) THE FIX
for last session's deadlock: caller's run was a BLOCKING tcp_get that froze netd's
loop + DROPPED the child's /host frames at the caller's own export. tcp_run = a
client conn that PUMPS the event loop while waiting: accumulates run output, feeds
non-run frames to on_frame (serves child's /host reads DURING the run), pumps server
conns (pump_conns), acks health-ping. Same 'netd must never block' as 4a capture,
now on the CALLER. Verified two-VM, zero aborts, non-cpu unregressed.
**PHASE 4 DONE.** Next: Phase 5 (explicit distributed compute - partition work +
inputs, ship, collect; genuine research/frontier), OR auth hardening, concurrent-
writer, full /net/tcp dial-out. v0.9.0 ready to cut (Phase 4). Other candidates: full /net/tcp (dial
OUT), auth hardening, concurrent-writer. See [[project-release-process]].

BRANCH STATE (unmerged/unpushed, STACKED, main at v0.8.0): `cluster-ns-aware-export`
(the resolver refactor) -> `cluster-phase4-cpu` (Phase 4 design + 4a impl). Merge/
release when ready.

**EXPORT-HARDENING PHASE (cluster auth) DONE (2026-08-26, branch
`cluster-auth-export-hardening`, commit 5aeeedf, COMMITTED but NOT tagged/pushed/
released - Hans chose "branch + commit only"; VERSION bumped to 0.10.0 in the
commit).** Chosen as the next step after Phases 0-4 (over Phase 5, which needs a
concrete workload) - the loudly-deferred trusted-LAN debt. Locks the 9P export
(TCP 564): every request (fs verbs AND cpu/NP_RUN) authenticated with a SHARED
CLUSTER SECRET via a client-nonce MAC - `mac = HMAC-SHA256(key, nonce||np)`, the
secret never on the wire, NO extra round trip (chosen over server-nonce
challenge-response because one-connection-per-request would cost an RTT per op).
Gate = netd handle_9p `authenticate()`; signer = `frame_signed()` in handle_rmount/
handle_run (covers mount -r, cpu, AND the /host reverse callback - free via the
SYMMETRIC key). Key read from \CLUSTER.KEY at boot via fsd client, fail-closed,
threaded as &Auth through the event loop (no mutable statics - .bss ceiling). A
\NOEXEC flag shares disk but refuses cpu. New: netd/src/hmac.rs (hand-rolled
SHA-256+HMAC, NIST/RFC 4231 validated), ninep-abi auth-frame consts, FS_ERR_AUTH
(u64::MAX-30, NOT -11 which collided with SPAWN_ERR_BAD_ELF - caught by rustc
unreachable_patterns). np9p_client.py/np9p_server.py now sign/verify (--key
override); Makefile stages dev CLUSTER_KEY=ouroboros-dev-cluster-key-v1. VERIFIED
cross-impl vs real guest (run-image-9p + np9p_client.py: correct key serves disk,
wrong key AUTH FAILED, zero faults). Lesson: a magic-byte transposition in BOTH
python peers passed the host-only Python<->Python test - only the independent Rust
guest caught it (foreign observer must be genuinely foreign). DEFERRED out loud:
per-peer identity, reply-direction (mutual) auth, replay protection. See
docs/cluster-auth-postmortem.md (the 14th). Two-VM interactive test not run
unattended (needs 2 terminals) - the Python foreign-observer cross-impl test is
stronger validation anyway. TODO if resuming: cut v0.10.0 release (tag/push/gh)
per [[project-release-process]] when Hans asks.

**DIAL-OUT (/net/tcp connection files) DONE (2026-08-26, SAME branch
`cluster-auth-export-hardening`, commit 06b165c stacked on the auth commit
5aeeedf, COMMITTED but NOT tagged/pushed/released; VERSION still 0.10.0 - a
dial-out release would be a further bump, Hans decides).** The last unshared
cluster resource: a machine dials TCP OUT OF ANOTHER's NIC ("use another's
network"), Plan 9 /net/tcp connection files. Chosen by Hans over concurrent-writer
/ auth-tier-2 / Phase 5. File model (netd /net/tcp, read-WRITE): read clone->N,
write "connect ip!port"|"close" to N/ctl, write/read N/data, read N/status. KEY
DESIGN: the connection HANDLE lives in the PATH (/net/tcp/N/) so NO fids needed
(Phase-0 path-based-verbs paid off); net_op ONLY mutates a bounded
[Option<DialConn>; MAX_DIAL=2] table, the EVENT LOOP does all TCP (pump_dials +
dial_on_segment, threaded like conns/auth); connect ASYNC (client polls status) -
the "netd never blocks its loop" rule again. Routing reuse: NsTarget::NetLocal
(local, mount -n /net) / NsTarget::Remote (dial out of A, /mnt/a/net/tcp/...);
ulib grew fs_write_inline + np_netlocal carries write data (/net was read-only).
New /bin/dial (programs/netutils/dial); np9p_client.py grew a `dial` op.
Stop-and-wait reliability (no cwnd/SACK). SCOPE INSIGHT: `cpu A fetch` already
dials out for run-a-program, so /net/tcp's real value is a RAW connection B
drives with no program on A. TRAP: guard-page STACK OVERFLOW on first boot
(2KB+4KB per-conn buffers x4 ~24KB on serve's stack - network arc bumped that
stack 3x, still not learned) - fixed by shrinking to 512/1024 x2. Verified vs a
host echo server (foreign observer): host drove guest's /net/tcp over the export
-> guest dialed 10.0.2.2:8000 out its NIC -> server saw the connection + got the
forwarded GET -> reply streamed back, 52 bytes, zero faults. Deferred: inbound
listen/accept, UDP. See docs/dial-out-postmortem.md (the 15th). Branch now has
TWO stacked arcs (auth 5aeeedf + dial-out 06b165c) unreleased on main (main at
v0.8.0... actually v0.9.0 is latest tag).
(per-task namespace + mount/bind syscalls, CWD-shaped), 0d (fsd multi-mount =
the payoff), 0e (cond on the verbs). Phase 0 done = v0.5.0. Of the independent
small items, large-read fsd restart is DONE (v0.4.1,
[[project-fsd-large-read-fix]]); GPT read-validation + pipeline redirect remain.

**UPDATE 2026-08-26: both stacked arcs SHIPPED.** The auth + dial-out branch was
ff-merged to main and released as TWO per-arc tags: **v0.10.0** (export-hardening
auth) and **v0.11.0** (/net/tcp dial-out), both live on GitHub. The "NOT
tagged/pushed/released" notes above are now stale - they're released. See
[[project-release-process]] for the two-stacked-arcs release mechanics.

**DIAL-IN (/net/tcp accept) DONE (2026-08-26, branch `cluster-dial-in`, commit
e056427, SHIPPED as v0.12.0 (2026-08-26, ff-merged to main, tagged/pushed/released)).** The mirror of
dial-out: a machine ACCEPTS inbound TCP on ANOTHER's NIC (`serve /mnt/a/net 9000`
announces on A; a client connecting to A:9000 is answered by a program where serve
runs; A=ingress, B owns the service). Completes /net/tcp symmetrically. File model:
write "announce <port>" to /net/tcp/N/ctl (Listening); read /net/tcp/N/listen -> #
of an accepted conn M; /net/tcp/M/data as usual. ALMOST ALL REUSE - an accepted
conn IS a DialConn; new = Listening+Accepting states + dial_accept + a listen scan
(handle still path-encoded, no fids). Ordering: dial_accept AFTER dial_on_segment
so a retransmitted SYN never double-accepts. Fixed a real bug: Closing sent only
the FIN, stranding a response written just before close -> pump_dials now flushes
send data in BOTH Established+Closing, FIN only once drained. New /bin/serve
(programs/netutils/serve); np9p_client.py grew a `serve` op; run-image-9p forwards
:5900->:9000. Verified vs a foreign observer (host socket -> guest:9000, got the
served reply byte-exact), zero faults. STACK OVERFLOW returned a 5TH time
(MAX_DIAL=4 blew serve's 32KB stack -> capped to 3 + rbuf 768). Scope caveat:
dial-in is MORE SPECULATIVE on consumers than dial-out (cpu A <server> runs a
server ON A; dial-in is for when state lives on B) - built to complete the model,
consumer question flagged. Deferred: persistent multi-accept loop, UDP. See
docs/dial-in-postmortem.md (the 16th). NOTE: dial-in shares one branch with
nothing else this time (clean off main at v0.11.0); ship as v0.12.0 when Hans asks.

**REPLY-AUTH (auth tier 2, part 1) DONE (2026-08-26, branch `cluster-reply-auth`,
commit f828282, SHIPPED as v0.13.0 (2026-08-26, ff-merged, tagged/pushed/released)).** Chosen by
Hans as the next arc AFTER dial-in, explicitly as defense-in-depth (he is NOT
planning a non-trusted LAN; the rest of tier-2 is roadmapped behind a "leaving a
trusted network" TRIGGER). v0.10.0 authed the export REQUEST; this auths the
REPLY too -> mutual auth (an active injector can't feed a client forged data).
Wire: framed reply gains a 32-byte MAC prepended -> [len][mac:32][status][result],
mac=HMAC(key, REQUEST_NONCE || [status][result]). Binding to the request nonce =
no new wire field (both sides hold it) + ties reply to request. netd: authenticate()
returns the nonce, seal_reply() wraps every framed export reply, frame_signed()
returns its nonce, handle_rmount verifies (FS_ERR_AUTH on mismatch). No round trip,
no state. np9p_client/server.py updated. KEY HONEST BOUNDARY stated everywhere:
INTEGRITY NOT CONFIDENTIALITY - cleartext on the wire, a sniffer still reads files;
encryption is a SEPARATE axis under the trigger. cpu-run output STREAM stays
reply-unauthenticated (not a framed reply; deferred). Verified vs the Python
foreign observer (guest seals, python verifies - correct-key readdir/dial/serve
pass; tampered reply rejected), zero faults. Written as a TIER-2 ADDENDUM to
docs/cluster-auth-postmortem.md (clean small follow-on, not a fresh postmortem).
TRIGGER-GATED remaining tier-2 (roadmap security section): replay protection,
per-peer identity, transport encryption, cpu-stream reply-auth - build ONLY when
Ouroboros leaves a trusted network. Ship reply-auth as v0.13.0 when Hans asks.

**CPU OUTPUT STREAMING (chunked pull) DONE (2026-08-26, branch
`cluster-cpu-streaming`, commit d288f4c, COMMITTED not pushed/released - candidate
v0.14.0). Chosen by Hans as the pragmatic "close out the cluster" arc.** cpu
output was capped at ONE IPC message (768B - the MSG_CALL reply); now the shell
PULLS it in chunks (up to ~2KB). Forced by the capability model: netd lacks
TO_SHELL so it CAN'T push a stream, only reply once via the reply exemption -> a
PULL loop. handle_run collects the run output into a PendingRun buffer (RUN_OUT_MAX
~2KB, on serve's frame; tcp_run writes straight into pending.buf so NO net stack
increase - the guard-page overflow was a real risk, verified clean), returns chunk
1; cmd_cpu pulls the rest with new op NETOP_RUN_MORE=6 (empty reply=EOF),
owner-checked, one pending run at a time. drain/handle_client take pending as
Option (tcp_run's re-entrant drain passes None - no nested run since caller
blocked). SCOPE: BOUNDED ~2KB (netd collects whole run before the shell pulls;
remote also accumulates before sending). TRULY UNBOUNDED STREAMING = a documented
later refinement in roadmap-cluster.md (remote sends as child produces + resumable
caller interleaving remote->caller->shell + maybe grant netd TO_SHELL to push) -
build if multi-KB cpu output is wanted. THE CLUSTER FEELS DONE (Hans's words: this
was the last obvious gap). Ship as v0.14.0 when Hans asks.
