# Phase 4 design — remote execution (the Plan 9 `cpu` model)

The design for **Phase 4** of [`roadmap-cluster.md`](roadmap-cluster.md): *run a
program on machine B while its namespace — files, console, devices — is imported
from machine A over 9P.* The program does ordinary file I/O against imported
trees, so **no shared memory is needed** — this is the achievable, explicit
"ship the computation to where the CPU (or the data) is," the honest form of
distributed processing (see [`roadmap-cluster.md`](roadmap-cluster.md)'s
"shared-memory wall").

This is the **frontier** phase; the scope choices matter, so it is staged
tightly. Grounded in the code as it stands after Phase 3 + the namespace-aware
export.

## The end state, and the smallest honest first step

**End state:** `cpu B <command>` on machine A runs `<command>` on B, and the
command reads/writes **A's** files and prints to **A's** console — B's CPU, A's
resources. That is two hard things at once: (1) **remote spawn** (A makes B run a
program and get its output), and (2) **namespace import** (the program on B sees
A's files/console). Doing both in one step is how you build the wrong thing.

**Step 4a (this design's first shippable):** **remote spawn + output stream, no
namespace import.** `cpu B <command>` runs a `/bin` command *on B, using B's
resources*, and streams its output back to A's console. This is the `ssh host cmd`
core — useful on its own, and the load-bearing mechanism the import step builds on.
The namespace import is **step 4b**, once 4a's spawn/capture/stream is proven.

## Who spawns on B — and why it's `netd`

`SPAWN` is not task-gated: any task can read an ELF from `fsd` and spawn it
(that's exactly what the shell's `spawn_path` does — chunk the binary via
`SPAWN_STAGE`, stage argv/cwd, `SPAWN` with a `stdout_target`). So B needs *a*
task to receive the run request and spawn. The candidates:

- **`netd`** — it already owns the network (the run request arrives over TCP), is
  already an `fsd` client (it can read the `/bin` binary), and its event loop
  already drains client messages every wake. **Chosen.**
- A new dedicated `cpu` server task — a new protected slot, duplicating netd's
  TCP and fsd-client plumbing for no gain today.
- The shell — interactive (task 0, the keyboard owner); wrong shape.

**The capture must not block netd.** The shell captures a child by *blocking* in
`MSG_RECV` until the child's end-of-stream (`capture_program_output`). netd cannot
block — it is supervised, health-pinged, and services many connections. But it
doesn't need to: **a spawned child's output arrives as ordinary messages to
`NET_TASK`** (the child's `stdout_target` is netd, so `pipe_out` sends netd
`MSG`s), and netd's event loop *already drains its mailbox every wake*. So the
capture is: spawn the child with `stdout_target = NET_TASK`, remember `(child slot
→ the requesting TCP connection)`, and in the event loop forward any message
*from the child's slot* out over that connection; on the child's empty
end-of-stream message, `WAIT`-reap it and FIN the connection. **The child runs on
B's scheduler; netd only relays** — no blocking, fits the existing model.

## The four design decisions

### Decision 1 — the run request is a new framed verb on the export connection

**Decision.** `cpu B:564 <command>` opens a connection to B's export port (564) and
sends a **run frame** — a new framed message alongside the NP verbs:
`[len][RUN][argc][arg bytes...]`. netd's `handle_9p` dispatches it (verb in the
NP-range check, or a distinct opcode) to the spawn path. The reply is not a single
NP reply but a **stream**: the command's output bytes, then a clean FIN = "done"
(the exit status can ride a small trailer later). A's `cpu` reads the stream to
FIN and prints it, exactly as `tcp_get` already reads an HTTP/9P reply to EOF.

**Rationale.** Reuse the whole transport (the 564 listener, `tcp_get` on A's side,
the connection state machine on B's). A run is "a request whose reply is a
stream," which is what the HTTP file server already does (`pump_send`).

**Alternative rejected.** A separate TCP port / a `NETOP_*` local op. The export
connection already carries "do something on the remote and stream the result."

### Decision 2 — step 4a runs with B's namespace (no import); 4b imports A's

**Decision.** In 4a the spawned command inherits **netd's / B's** namespace — it
reads B's `/bin`, B's disk. `cpu B ls /` lists **B's** root; `cpu B cat /net/ip`
prints **B's** IP. That already proves "the computation ran on B." **4b** then sets
the child's namespace to a **remote mount back to A** (`bind /host → A:564`, or
rebind `/`), so `cpu B cat /host/RTEST/F.TXT` runs on B but reads A's file — the
namespace-import payoff. The transitive-re-export hook the namespace-aware export
left (`NsTarget::Remote` in an export namespace) is exactly this: B's spawned
child resolving a path to A.

**Rationale.** 4a's value is the spawn/capture/stream plumbing; layering the
namespace import on top is a *namespace-setup* change (set the child's `NS_SET`
before `SPAWN`), not a transport change. Separating them keeps each verifiable.

### Decision 3 — output routes back over the connection, not through a remote console

**Decision.** The command's stdout is **captured and streamed back** to A over the
run connection; A prints it. It is *not* routed to A's console by giving the child
a remote `/dev/cons` mount.

**Rationale.** Console output is `con_write` straight to `CON_TASK` — it
deliberately *bypasses* namespace resolution (the console-server postmortem's echo
hot-path). So a remote `/dev/cons` bind would not catch a program's normal
console writes. Capturing the child's `stdout_target` stream (the pipe mechanism
the shell already uses for `|` and `> file`) is the mechanism that *does* catch
them, and it is transport-agnostic — the same bytes go over TCP instead of to a
relaying shell. (A future step could route interactive stdin/stdout both ways.)

### Decision 4 — trusted-LAN, and a bounded, reaped child

**Decision.** As with every cluster phase: **trusted-LAN, no auth** — B runs what
A asks. The child is a normal spawnable-slot task: it is `WAIT`-reaped when its
stream ends, killed if the connection drops mid-run (no orphan), and subject to
the same slot limit as any spawn (a busy B refuses with a clean error). netd
delegates the child only the capabilities it needs (`TO_FSD` to read files; the
stdout pipe to `NET_TASK`), the same targeted delegation the shell does for `/bin`.

**Rationale.** Remote code execution is the sharpest edge in the whole arc; it is
gated by the same honest trusted-LAN posture, made explicit, with the child fully
accounted for (reaped, killable, capability-scoped) so a remote run can't leak a
slot or a capability.

**A capability constraint found while designing, load-bearing for 4a.** The
child's output pipe (`stdout_target = NET_TASK`) needs the child to *send to
`NET_TASK`* — a runtime capability. Today only the **shell** hands that out
(`delegate_net`), because `DELEGATE` lets a task delegate only a send-cap it
**statically holds** (`may_delegate`), and a `/bin` slot statically holds
`TO_FSD`/`TO_CON` but *not* `TO_NET`. netd spawning the child is not the shell, and
netd delegating "send to `NET_TASK`" (itself) is a self-send cap it does not
statically hold. **So 4a needs a capability-model addition before the output pipe
works** — the cleanest being a **spawner→child reply capability** (a child may
always send to the task that spawned it, the natural "parent" relationship),
granted by `SPAWN` itself rather than a separate `DELEGATE`. This is the first real
work item of 4a, and the reason it is not a quick add: the transport and spawn
mechanics are straightforward, but *letting the child talk back to netd* touches
the capability model. (Alternative considered: give spawnable slots a static
`TO_NET` — rejected, it widens every command's authority for one caller's benefit.)

## Staging

- **4a — remote spawn + output stream. ✅ DONE (2026-08-26).** The `NP_RUN` frame;
  netd's spawn path (read the `/bin` ELF via `read_file_chunk` + `SPAWN_STAGE` in
  512-byte chunks — the `MAX_USER_LEN` cap — stage argv, `SPAWN` with
  `stdout_target = NET_TASK`, then `DELEGATE` the reply cap); the event-loop
  capture (`drain_client_messages` routes a message *from the child's slot* to its
  connection, reaps on end-of-stream); A's `NETOP_RUN` + the `cpu <host:port>
  <command>` shell builtin. The capability piece: `NET_TASK` gained a self-send
  `TO_NET` bit *only* to delegate the reply cap to a spawned child. *Shipped:* on
  two VMs, `cpu 10.0.2.10:564 ls /` prints A's root including the A-only `RANHERE/`
  marker (proving it ran on A), `cpu … uptime` prints A's uptime, a bad command a
  clean error; the serve-loop restructure (iterate conns by index so the drain can
  route cpu output) regression-tested against the full export matrix, zero aborts.
- **4b — namespace import. ✅ DONE (2026-08-26).** The command runs on the remote's
  CPU but reads the *caller's* files, through the caller's namespace imported at
  `/host`. The `cpu` frame carries the caller's endpoint (in the `NP_RUN` a1/a2);
  the remote netd binds `/host → remote(caller)` on its own namespace (`NS_SET`)
  before `SPAWN`, and the child inherits it. Three pieces:
  - **The `/host` bind + endpoint** — small.
  - **The child-message demux** — the child now talks to netd for *two* things
    (stdout `MSG_SEND`, `/host` fs `NETOP_RMOUNT` `MSG_CALL`), told apart safely by
    the op field: a fs call always carries `NETOP_RMOUNT`, so it's never mistaken
    for output (which would deadlock the child).
  - **The caller-side non-blocking run (`tcp_run`)** — the real work, resolving the
    deadlock found last session. `cpu B cat /host/F` needs the child to read the
    caller's file *while* the caller runs the command, but a blocking `tcp_get`
    (the old `handle_run`) froze the caller's event loop and *dropped* the child's
    `/host` frames arriving at its export. `tcp_run` is a client connection that
    **pumps the event loop while it waits**: each pass it accumulates the run's
    output, feeds every non-run frame to `on_frame` (serving the child's `/host`
    reads *during* the run), pumps the server connections so those replies go out,
    and acks the health-ping. The "netd must never block" rule (4a's capture),
    now on the caller.

  *Shipped:* two VMs, a file on B only, then from B `cpu 10.0.2.10:564 ls /` (A's
  root), `ls /host` (B's root, showing the B-only file), and `cat /host/BONLY.TXT`
  (its contents) — the command on A reading B's disk through the import. Zero
  `-d int` aborts; non-cpu paths unregressed. **Phase 4 (4a + 4b) complete — the
  full Plan 9 `cpu` model.**
- **4c (later) — interactive stdin, exit status, environment**; and, further out,
  the honest **explicit distributed compute** of Phase 5.

## Risks / deferred

- **netd stability.** netd is supervised and delicate; the spawn/capture must stay
  non-blocking (event-loop-integrated) so a slow or wedged child never starves the
  health-ping. The child runs on its *own* slot; netd only relays. If it strains,
  the fallback is a dedicated cpu server task (Decision 1's rejected alternative).
- **Distinguishing child output from client requests.** A child's messages arrive
  at `NET_TASK` like any client message; netd tells them apart by **sender slot**
  (the spawned child's), routing those to the run connection and everything else
  to the normal client dispatch.
- **Flow control for the output stream.** The child produces output faster than
  the TCP window drains; netd must pace (the `pump_send` window logic) or bound
  the child (it blocks in `pipe_out` when netd's mailbox is full — natural
  backpressure). Measure before optimizing.
- **A busy remote / a dropped connection.** No free slot → a clean "remote busy"
  error; connection drop mid-run → kill + reap the child (no orphan).
- **No auth, no resource limits.** Trusted-LAN (Decision 4); CPU/memory limits on
  a remote run are a later hardening concern.

## Effort

4a is the largest netd addition since the network stack itself — a spawn/capture/
stream subsystem — but it decomposes: the spawn path mirrors `spawn_path`, the
capture rides the existing mailbox drain, and the stream rides the existing
`pump_send`. 4b is a namespace-setup delta on top. A completed 4a+4b → the first
real **"run there, resources from here"**, and the milestone that makes Ouroboros
a *compute* cluster, not only a storage one. → some **v0.9.0**.

## Sources

- [`roadmap-cluster.md`](roadmap-cluster.md) Phase 4 and the shared-memory-wall
  framing; Plan 9's `cpu` command and CPU-server model.
- The namespace-aware export ([`roadmap-cluster-phase3.md`](roadmap-cluster-phase3.md))
  whose `NsTarget::Remote`-in-an-export-namespace hook is 4b's mechanism.
- The shell's `spawn_path`/`capture_program_output` (the spawn+capture flow this
  ports into netd) and `pump_send` (the output-stream pacing).
