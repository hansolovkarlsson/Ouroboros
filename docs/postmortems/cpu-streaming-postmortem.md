# `cpu` output streaming — the chunked-pull postmortem (the seventeenth)

*A design retrospective, 2026-08-26 — the pragmatic "close out the cluster" arc.
`cpu <host:port> <cmd>` had one nagging limitation: its output was capped at a
single IPC message (768 bytes), so anything chatty truncated. This lifts that to
~2 KB with a chunked pull. Small in code, but it carries three lessons worth
keeping: a design shaped by the capability model, a stack trap dodged on purpose,
and a testing capability that changes what's verifiable.*

## The capability model chose the design, not me

The instinct was "just send the output in more messages." But the capability
model said no in a specific, instructive way: **netd does not hold `TO_SHELL`.**
Its send-mask lets it reach `fsd` and `cond` and the NIC, and it may *reply* to a
client blocked in a `MSG_CALL` to it (the reply exemption) — but it cannot
*initiate* a send to the shell. So netd physically cannot push a stream of output
messages at the shell; it can only answer one request with one reply.

That single fact eliminated the "push" design entirely and forced a **pull loop**:
the shell asks for the next chunk (`NETOP_RUN_MORE`), netd answers one chunk via
the reply exemption, repeat until an empty reply. netd holds the collected run
output in a `PendingRun` buffer between the pulls, with an owner check so only the
task that started the run may pull it.

The lesson is one this project keeps relearning from a new angle: **the isolation
model is a design input, not an obstacle to route around.** The same
"who-may-send-to-whom" mask that makes the system safe also dictated the shape of
this feature — and the pull loop it forced is arguably *better* than a push (the
shell paces the delivery, and there's no way for netd to spam a task that isn't
listening). I could have loosened the mask (grant netd `TO_SHELL`) to enable a
push; choosing not to, and letting the constraint pick the design, kept the
isolation property intact for a feature that didn't need to weaken it.

## The stack trap, dodged on purpose this time

Five times now the guard-page stack overflow has bitten this project when a buffer
grew on a `no_std` server's fixed stack (the network arc thrice, dial-out,
dial-in). This arc added a ~2 KB `PendingRun` buffer to `netd`'s already-tight
`serve` frame — exactly the setup that has overflowed before. This time it *didn't*,
and not by luck: the buffer was placed so that `tcp_run` writes the run's output
**straight into `pending.buf`** instead of into a local `resp` buffer it used
before. The 2 KB moved from a transient stack frame (alive only during a run) to a
persistent one — but the *peak* stack, which is what overflows, happens *during* a
run, so the net change at the peak was ~zero. First boot: clean, no fault.

The lesson: after being burned this many times, the reflex finally inverted —
*before* adding the buffer I asked "where does this live at peak stack, and can I
reuse space that's already there?" rather than adding it and finding out at boot.
The trap didn't stop biting because the code got more careful by accident; it
stopped because it earned a checklist item.

## The re-entrancy subtlety that `Option` made honest

netd's `drain_client_messages` is called from two places: the main event loop, and
*re-entrantly* from inside `tcp_run` while a `cpu` run is in flight (to keep
serving the child's `/host` callbacks). If that re-entrant drain processed a
`NETOP_RUN` or `NETOP_RUN_MORE`, it would be a nested run or a pull mid-run — which
**cannot legitimately happen**, because the shell that started this run is blocked
in it and can't send another. Rather than trust that invariant silently, the
`pending` argument is an `Option`: the main loop passes `Some`, `tcp_run`'s drain
passes `None`, and the run-op handlers no-op on `None`. The type now *states* "run
ops aren't serviceable from the re-entrant path," instead of relying on a comment
and a hope. A small thing, but it's the difference between an invariant that's
documented and one that's enforced.

## The testing capability that changes what's verifiable

`cpu` needs a *remote* (you can't cpu yourself — ARP-ing your own IP gets no
reply), and it runs *inside the guest shell*, so verifying it seemed to need two
interactive VMs. The unlock was discovering that **QEMU `-nographic` delivers piped
stdin straight to the guest shell** — the console read pulls from the PL011 UART,
which `-nographic` wires to the process's stdin. Feed a FIFO, `echo` a command into
it, and the guest runs it unattended.

With that, the whole `cpu` path was verified on a *single* guest: drive `cpu
10.0.2.2:5641 …` against a host "fake export" that streams 1500 bytes back, and
confirm the guest prints all 33 lines — past the old 768-byte cut. No second VM,
no manual terminals. This is a genuine expansion of what can be tested
automatically: previously only the export side (host driving the guest over TCP)
was scriptable; now anything that runs *in* the guest shell is too. The technique
outlives this arc.

## Scope held, and named

This is **bounded** to ~2 KB: netd collects the whole run before the shell pulls
it, and the remote itself accumulates the child's output before sending. That
covers the small commands `cpu` is really for. Truly unbounded streaming — the
remote sending as the child produces, a resumable run interleaving
remote→caller→shell — is a real arc (three moving parts), written up in
`roadmap-cluster.md` as a later refinement to build *if* multi-kilobyte `cpu`
output is ever actually wanted. Shipping the bounded version and naming the
unbounded one is the same "smallest useful thing, boundary documented" discipline
the whole cluster was built on.

## The one-line lesson

*Let the isolation model pick the design (it chose a pull loop, and chose well);
put a growing buffer where the peak stack already is, not where it's convenient;
and a constraint you make the type system state beats one you leave to a comment.*
