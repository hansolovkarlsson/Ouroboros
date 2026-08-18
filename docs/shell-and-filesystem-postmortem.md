# From a bare shell prompt to a real, disk-backed OS: an ARM64 kernel postmortem

This is the second piece of a four-part debugging history for
[Ouroboros](../README.md), a from-scratch ARM64 microkernel.
[The first piece](boot-bringup-postmortem.md) covers getting from "UEFI
hands us control" to a live, working console on real hardware.
[The third](xhci-keyboard-postmortem.md) covers getting real keyboard
input into that console, and [the fourth](usb-storage-postmortem.md)
covers getting this document's filesystem a real disk on real
hardware, over USB. This one covers everything in between: turning
a bare shell prompt into a real, disk-loaded userland process with real
commands, a real filesystem underneath it, and real write support - and
the bugs found doing it, several of which are genuinely reusable
lessons for anyone else writing a small freestanding kernel.

One theme dominates this stretch more than any single bug: **a whole
family of crashes that all trace back to the same root cause**, found
three separate times before the underlying pattern was fully understood.
That's worth reading as its own throughline, not just three unrelated
war stories.

## The recurring bug: pointers baked into your binary don't know where they'll actually load

Ouroboros's userland programs are loaded as flat, position-*dependent*
binaries - no ELF, no relocation processing, linked with a fixed base
address of `0x0`, then copied byte-for-byte to wherever the loader
actually placed them in memory. Direct function calls are fine: AArch64
compiles a call as a PC-relative branch, correct no matter where the
code actually ends up. The problem is anything that isn't a direct call
- any *pointer stored as data*, computed once at compile/link time and
baked into the binary's read-only data section. That pointer was
computed against the link-time base of `0x0`. Nothing ever adjusts it
for the real load address, because there's no relocation step at all.
This bit twice, in two completely different-looking ways, before the
general shape of the problem was understood.

### First bite: `core::fmt`/`write!` crashes any loaded program

**Symptom:** a new `uptime` command's very first implementation used
Rust's ordinary `write!(writer, "{ticks} ticks")` to format a number -
and crashed immediately, `ELR_EL1` landing on a tiny, near-zero address
instead of any real code.

**Root cause:** `core::fmt`'s formatting machinery doesn't dispatch
argument formatting through direct calls - it builds an array of
function pointers, one per formatted argument, as *data*, baked into
the binary's read-only section at compile time. For a normal, properly
relocated executable this is invisible; for a binary linked at base
`0x0` and loaded somewhere else with zero relocation processing, those
pointer values are simply wrong wherever the code actually runs -
jumping through one lands on whatever garbage happens to sit at "the
real load address's own base," near-zero if the load address itself is
near-zero-relative in the crash's own frame of reference.

**Fix:** hand-roll the one piece of formatting actually needed (decimal
printing of a `u64`) instead of going through `core::fmt` at all. Not
elegant, but the only viable fix short of writing a real relocating
loader.

### Second bite: even a plain slice/string comparison against a literal can crash

**Symptom, much later, in completely unrelated code:** a `cd` command's
path-resolution logic needed to check whether the current directory was
already root - a plain comparison, `cwd_bytes != b"/"`, comparing a byte
slice against a literal. It crashed, with the exact same signature:
`ELR_EL1` inside the program's own code, `FAR_EL1` a small, garbage,
code-layout-dependent address that shifted between builds as debug
prints were added and removed.

**Root cause:** the identical underlying problem, in a form that's much
easier to miss than a formatting macro. Comparing a slice against a
byte-string literal needs a *reference* to that literal's bytes,
somewhere in memory - and a literal's storage location is exactly the
kind of thing computed once, at compile time, relative to the link-time
base. Direct calls and comparisons against *scalar* values (a length, a
single byte) never need this; anything that needs a pointer to literal
*data* does.

**Fix:** replace every slice/string comparison against a literal with
scalar comparisons instead (checking length and individual bytes
directly) - which never trip this, because they never need a reference
to anything.

**How this one was actually found:** by bisection, not inspection -
binary-searching temporary print statements through the path-resolution
function until the exact crashing line was isolated. The bug's *symptom*
(a garbage `ELR_EL1`) looked nothing like its cause (a plain string
comparison) until that isolation was done.

### The generalized rule this produced

Once both bites had happened, the practical rule became explicit and
has held ever since: **no `core::fmt`/`write!`, and no slice/string
comparison against a literal, in any loaded program** - both crash for
the identical reason, a data reference computed for a link-time base
that's never actually where the code runs. Direct function calls and
comparisons against scalar values are fine. This is also exactly why a
later, much larger addition - a shared crate holding syscall numbers and
sentinel values used by both the kernel and every userland program -
needed its own explicit safety argument, not just "it's simple so it's
probably fine": every value in that crate is a scalar constant, which
the compiler inlines as an immediate operand at the use site, fundamentally
different from a *pointer* to literal data. A future extension that
adds anything to that shared crate needs to preserve that property, not
just assume it.

> **Lesson:** if you're loading position-dependent code with no
> relocation step, don't just audit for "did I call `write!` anywhere."
> Audit for *any* pointer to compile-time data - format strings,
> literal comparisons, static tables of function pointers, vtables -
> anywhere the compiler might silently reach for one on your behalf.

## Turning the shell into a real userland process

Before any of the above, the shell (line editor and command dispatch)
was just kernel code running at a low privilege level, compiled directly
into the kernel image. That doesn't match how any real Unix-like system
works, so a deliberate architecture change made it a genuine, separate
program: loaded from disk, selected by a small config file, replaceable
without rebuilding the kernel at all.

**A real scope decision, made explicitly rather than assumed:**
"loaded from disk" didn't have to mean a real *runtime* block-device
driver. UEFI's own filesystem driver on the boot partition already
reads files just fine during boot services - the same window the
kernel's own binary gets loaded in - so the loader reads a config file
and a program binary that way, entirely before boot services end. A
real runtime block-device-plus-filesystem stack (needed for anything
loaded *after* boot, like a real `exec()`) stayed explicitly deferred,
not attempted prematurely.

This surfaced two real build-system problems, both eventually fixed:
a plain build command at the repository root tried to build the new
userland-program crate too, using the wrong target - fixed by making
the kernel the workspace's only default build member, so the userland
crate stays a full workspace member (shared lockfile, builds correctly
with the right target specified) without being pulled into a bare
build invocation; and producing a raw flat binary from the linked
executable needs a binary-copy tool that doesn't ship with this
platform's default toolchain by default, needing a small amount of
detective work to find its real, fixed install location without adding
an entire extra dependency for one invocation.

**A genuinely nice side effect:** this killed an earlier, hard-won
compiler-alignment ceiling entirely (see [the boot bring-up
postmortem](boot-bringup-postmortem.md) for that story) - a disk-loaded
program doesn't need a compile-time-aligned static at all, since the
loader can just ask for however many runtime-determined pages a program
needs.

**A new alignment problem immediately took its place.** The page
allocator only guarantees 4KB alignment, not the coarser alignment the
identity-map builder's own block-splitting logic implicitly relied on.
Solved by deliberately over-allocating one extra block's worth of pages
and freeing whatever falls outside the first correctly-aligned address
in that range - recovering the same non-straddling guarantee the old
compile-time static used to get for free, just computed at runtime
instead.

## A measurement trap: why the "actual" tick rate was a lie for ten seconds

Making the shell feel responsive surfaced a real, if mundane, bug: typed
characters could take up to a full second to echo. Root cause: the
kernel's round-robin task switch fires unconditionally on *every* timer
tick, whether or not either task actually has anything to do - so a
keystroke arriving while the idle task happened to be scheduled would
sit untouched until the next tick swapped back. Lowering the tick
interval fixed the practical symptom (worst-case latency dropped to
imperceptible).

The interesting part is how the *diagnosis* nearly went sideways.
**The first attempt to measure the real tick rate used a whole-run
exception count from the emulator's own internal trace** - and
concluded the tick was already firing every 20-40ms, even *before* the
fix, which would have meant the entire latency theory was wrong. That
number was an artifact: UEFI firmware's own boot-time code - PCI
enumeration, device dispatch, all still resident and executing at the
same privilege level for the first several seconds of every run, well
before the kernel installs its own exception handling - takes real
interrupt exceptions of its own, at a much higher rate than the kernel
ever will. In one measured window, the overwhelming majority of
recorded exceptions traced back to a single address well outside the
kernel's own code entirely - boot noise, not ticks. A targeted debug
print placed directly inside the timer re-arm function - immune to this
contamination, since firmware never calls it - gave the true number,
which matched the configured interval exactly.

> **Lesson:** a whole-run exception or event count from a shared trace
> facility silently includes activity that has nothing to do with the
> code you're actually testing. Either start counting only after a
> known post-boot marker, or instrument the specific code path you
> actually suspect, directly.

## The disk that lied about its own format

Bringing up runtime disk I/O needed two real drivers - a virtio block
device and a hand-rolled FAT32 reader - and surfaced two more genuinely
confusing, easy-to-repeat mistakes.

**A misleading diagnostic tool, not a misleading device.** Figuring out
which virtio transport a block device had actually attached over needed
inspecting the emulator's own internal device tree through its monitor
interface - and the first attempt at that appeared to show *no block
device at all*, which would have meant the boot partition was
unreachable by anything, obviously wrong since the kernel demonstrably
boots from it every run. The real bug was in the diagnostic script, not
the emulator: reading the monitor's response with one fixed sleep
before switching to non-blocking reads cut the response off
partway through, consistently at the same byte count - which is exactly
what makes a truncation bug look like real, complete data instead of an
obvious error. Polling with an idle timeout instead of a fixed sleep
fixed the tool and revealed the real, complete answer immediately.

**The fast development disk format isn't what production images use.**
The emulator's own convenience disk-passthrough driver, used for the
fast day-to-day development boot loop, turned out to produce FAT16, not
FAT32 - confirmed by decoding the on-disk format's own header fields by
hand rather than trusting the tool's name. The actual production disk
image (built by a real disk-image tool, the same path real testing
hardware ultimately boots from) is genuinely FAT32. A separate boot
target using the real image was added specifically because the fast
dev-loop disk can never satisfy a genuine FAT32 reader - not a bug in
the reader, a real format mismatch between the fast loop and everything
else.

**The project's own directory name didn't fit the filesystem's own
naming limit.** FAT's short-name convention caps a path component at
eight characters plus a three-character extension; this project's own
boot-configuration directory name was nine characters. Real FAT32
formatters handle this by writing an additional long-filename directory
entry alongside a shortened alias - which a hand-rolled, no-allocator
FAT32 reader (a real, hard constraint: everything post-boot-services
runs with no heap available, and every off-the-shelf FAT implementation
surveyed assumed one was reachable somewhere) deliberately doesn't
parse. Rather than implement that parsing to accommodate one
avoidable nine-character name the project itself controlled, the
directory was simply renamed to fit - cheaper and more honest than
adding a real feature to work around a self-inflicted naming choice.

## The hang with zero output: an unsigned underflow, masked interrupts, and a runaway loop

This is the single most instructive bug in this whole stretch, because
of *how* it failed, not just why.

**Symptom:** navigating up two directory levels in a row - `cd ..`
twice from a two-level-deep path - hung the entire system. Not a crash,
not an error message, not a reported exception. Nothing. The whole
machine simply stopped responding, forever.

**Root cause:** a real, on-disk FAT32 convention this reader hadn't
accounted for. A subdirectory's own "parent directory" entry
conventionally stores cluster number `0` to mean "the root directory,"
rather than root's own actual cluster number. The path-resolution code
didn't know this special case, so that `0` flowed directly into the
sector-address calculation, which computes a real cluster's disk
address by subtracting a small constant from the cluster number. `0`
minus that constant, in unsigned arithmetic, wraps around to an
enormous, nonsensical sector number instead of a negative one - and the
resulting disk read against that garbage address simply never
completed.

**Why this produced *silence*, not a crash:** the read happens inside a
system call, and taking any exception - including the timer interrupt
that drives this kernel's entire preemptive scheduler - is masked from
the moment a system call is entered until it returns. A runaway loop or
stuck operation that would ordinarily eventually get preempted and at
least let the rest of the system keep running instead had nothing that
could ever interrupt it. The failure was completely invisible because
the one mechanism that could have surfaced it was, by design, exactly
the thing turned off for the duration.

**Fix:** substitute the real root cluster number wherever a resolved
directory entry's cluster number comes back as `0`.

**How the exact trigger was isolated:** direct, deterministic
real-system testing, narrowing one step at a time - a single `cd ..`
from a two-level-deep path worked fine every time; a *second* one
immediately after it hung every time, before the fix, with no
variance. That reproducibility is what made the underflow theory
checkable at all, rather than a guess among several.

> **Lesson:** a bug inside any code path that runs with interrupts (or
> your only preemption mechanism) masked doesn't just risk a hang - it
> risks a hang with *zero diagnostic signal*, because the same
> mechanism you'd normally rely on to at least keep observing the
> system is unavailable for exactly as long as the bug is active. Treat
> unsigned-arithmetic-adjacent, disk-derived, or otherwise
> externally-sourced values with real suspicion in any code path that
> runs this way, even ones that "can't realistically be zero."

## Adding real write support, deliberately, one narrow case at a time

With reads working, the project crossed into real filesystem writes -
directory creation and removal, then file creation, deletion, content,
copy, and move - each added as the narrowest useful case, in a
consistent order-of-operations discipline that's worth naming
explicitly: **claim a resource before using it, and check every
precondition before writing anything.** Creating a directory
allocates and marks its cluster claimed in the file table *before*
anything else touches disk, specifically so a failure partway through
leaves the cluster correctly claimed rather than silently reusable;
removing one checks that it resolves to a directory, isn't root, and is
genuinely empty *before* freeing anything, so a rejected request never
partially applies.

Two more real bugs surfaced in this arc - both caught by **reasoning
through the design before writing any test**, a genuinely different
discipline from every hardware-confirmed bug elsewhere in this history,
and worth naming as its own technique:

- **A latent bug caught before it could ever fire.** The
  cluster-`0`-means-root substitution from the hang bug above had been
  applied to *every* resolved path component, not just directories -
  harmless at the time, since nothing the reader could see had ever
  legitimately had cluster `0` other than a parent-directory entry. Empty
  file support was about to make "cluster `0`, and it's a real file, not
  root" a common, ordinary case - and without a fix, resolving a path to
  a freshly created empty file would have silently rewritten its cluster
  to root's own, and deleting it would have tried to free *root's own*
  cluster in the file table, corrupting the entire filesystem's root
  directory on the very first use. Fixed by gating the substitution on
  the entry actually being a directory - caught during design, before any
  test could have hit it.
- **The identical class of bug, on a write path this time.** Moving a
  directory to a different parent means its own "parent" entry now
  points at the wrong place unless something updates it - the same
  cluster-`0`-means-root convention, now needing to be *written*
  correctly instead of just read correctly. Caught the same way, by
  tracing through what the operation needed before implementing it, not
  by hitting a crash first.

A real argument-validation bug rounded this arc out: writing a file
with no content at all is a legitimate, meaningful case (truncate it to
empty), but a generic "is this pointer/length pair sane" check
originally rejected any zero-length buffer outright - correct for an
*output* buffer, where zero length is pointless, wrong for *input* data,
where empty is a real value the caller might genuinely mean. Fixed with
a second, deliberately narrower validation rule used only where an
empty value is meaningful.

## Techniques that generalized well

- **When two bugs share an exact crash signature in otherwise unrelated
  code, suspect a shared root cause before debugging either one from
  scratch.** The `core::fmt` bug and the literal-comparison bug looked
  nothing alike on the surface and were, underneath, the identical
  problem.
- **Bisect with temporary print statements when a crash's location and
  its cause are far apart.** Binary-searching through a function's own
  statements found the literal-comparison bug faster than reasoning
  about it in the abstract would have.
- **A tool that reads a live process's own diagnostic interface can lie
  just as easily as the process itself.** The `info qtree` truncation
  bug was in the *test harness*, not the thing under test - always worth
  considering when a measurement looks unbelievable.
- **Decode a foreign on-disk format's header fields by hand before
  trusting what a tool calls it.** "FAT32" and "FAT16" are both valid,
  common answers to "what format is this disk," and only one of them
  was true here despite the tooling's own naming.
- **A bug inside a no-preemption code path can hang with zero signal.**
  If your kernel masks interrupts during system calls (or anything
  similar), a stuck loop there won't just hang - it'll hang invisibly,
  which changes how suspicious you should be of unsigned arithmetic and
  externally-sourced values in exactly that code.
- **Not every bug needs a crash to be found.** Tracing through a
  design's own logic before writing a test caught two real, serious
  bugs before they ever had a chance to corrupt anything - a different,
  equally legitimate discipline from the hardware-confirmed debugging
  that dominates the rest of this project's history.

## Where this ended up

A real, disk-loaded userland shell - not kernel code - with real
tokenized commands, a real hand-rolled FAT32 filesystem underneath it,
and a full read/write file-management surface: listing, reading,
creating and removing both directories and files, writing real content,
copying, and moving. Everything in [the xHCI keyboard
postmortem](xhci-keyboard-postmortem.md) that follows this was built on
top of a shell that, by the end of this arc, was already a genuinely
real program running on genuinely real storage - just still waiting for
anything to reach it besides a piped test harness.
