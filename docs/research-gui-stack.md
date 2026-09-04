# Research: writing our own GTK and/or SDL — what it would actually take

The question, asked plainly: *would it be difficult to develop our own GTK
and/or SDL?* The short answer is that these are **two very different
questions**, and the hard part of both lives *underneath* the toolkit, not in
it. An SDL-shaped layer is feasible and half-built already; a GTK-shaped
toolkit is a real arc whose cost is almost entirely in the substrate it
presumes exists.

This note is forward-looking design reasoning, a companion to
[`research-directions.md`](research-directions.md) and
[`research-redox-and-pi.md`](research-redox-and-pi.md). It expands roadmap
item **f** ("Graphics card / GPU support") in
[`ROADMAP.md`](ROADMAP.md) from *"note virtio-gpu as the entry point"* into
*"here is the whole stack, and here is which layer is actually blocking."*

**Ouroboros's own state below is drawn from the tree, not from memory** —
`kernel/src/framebuffer.rs`, `kernel/src/fbdev.rs`, `kernel/src/fbconsole.rs`,
`kernel/src/font.rs`, `kernel/src/xhci.rs`, `kernel/src/syscall.rs`,
`programs/servers/cond`, and `libc/`.

## SDL-shaped: feasible, and largely already built

Strip SDL of the parts this system can't use anyway:

- **OpenGL / accelerated rendering** — no GPU, no mode-setting; the only
  "graphics" is the boot-discovered GOP linear framebuffer.
- **`SDL_Audio`** — there is no sound driver of any kind. Separate hardware arc.
- **Threads, `dlopen`, timers-as-signals** — the libc personality isn't near
  this depth (`libc/src/stdlib.c`'s `malloc` is a bump allocator over `sbrk`
  whose `free` is a documented no-op).

What's left is: **a surface, a blitter, an event queue, a timer.** And most of
that exists. `fbdev.rs` is already the dumb 2D blitter (plot a run of glyph
bitmaps, scroll, clear). `cond` already owns the framebuffer, already speaks a
drawing protocol to the kernel over the gated `FB_*` syscalls, and already
carries its own copy of `font.rs`. An SDL-shaped *client library* on top of a
draw server is a few thousand lines of userland — small, once the server below
it exists.

## GTK-shaped: the widgets are the easy 10%

A useful subset — button, label, text field, list, scrollbar, menu, a layout
pass, event dispatch — is a real but tractable arc, and an incremental one:
every widget is independently useful the day it lands.

What makes "GTK" expensive is everything it *presumes already exists*: a window
system, input routing and focus, a font stack with proportional and
antialiased text, clipboard, IME, double-buffering, theming. None of that
exists here yet. Writing the widgets is not the project; building the four
layers below them is.

## The five actual blockers, in order

### 1. No bulk-transfer path — the decisive one

`syscall_abi::MSG_MAX_LEN` is **768 bytes** (`kernel/src/syscall.rs:236`), and
there is **no shared memory** anywhere in `kernel/src` — no `shm`, no
`map_shared`, no page-granting primitive. Everything moves inline through
messages, by design (the isolation-and-dataflow arc).

A 1920×1080×32bpp frame is **8 MB** — about **11,000 messages**. Even a modest
dirty-rect update on a 640×480 surface is thousands. **SDL's entire model —
"here is a pixel buffer, I drew into it, present it" — is the wrong shape for
this ABI.** This is the finding that reorders everything else: the blocker is
not that GTK is big, it's that there is no path for bulk pixels.

### 2. No mouse

`xhci.rs` is a USB HID **boot-protocol keyboard** driver only. The good news is
that a boot-protocol *mouse* is a 3–4 byte report off an interrupt endpoint —
the same mechanism, through the same code path, whose hard-won lesson
(*real data comes from the interrupt endpoint, not `GET_REPORT`*) is already
written down in that file's header. Genuinely small; the natural first step.

### 3. No font stack

`font.rs` is a **119-line 8×8 bitmap font**, one glyph = 8 bytes, fixed cell.
Real UI text means either committing to bitmap fonts at a few fixed sizes (dull
but cheap, and honest for a console-descended system) or a TrueType rasterizer
— `fontdue` / `ab_glyph` are `no_std`-friendly and would be the pragmatic
vendor-in. Shaping and i18n are a further step beyond that and can be deferred
indefinitely.

### 4. `FB_*` is gated to `CON_TASK` alone

By design — `fbdev.rs`'s syscalls are accepted from the console server and
nobody else, so no application can touch pixels today. The fix is **a draw
server**, not a wider gate: widening the gate would hand raw framebuffer
writes to arbitrary tasks and throw away the isolation property the console
arc was built to establish.

### 5. No double-buffering, no mode-setting

GOP linear framebuffer only: fixed geometry discovered at boot, no vsync, no
page flip, tearing on every update, and a slow byte-by-byte blit path. Roadmap
item **f** already names **virtio-gpu over the existing `virtio_mmio`
transport** as the realistic entry point — the same DMA-in-the-kernel /
protocol-in-userland split as virtio-net and virtio-blk.

## The architecture that actually fits: `/dev/draw`, not `SDL_Surface`

Don't clone SDL's model. Clone **Plan 9's `/dev/draw`** — which this system is
already three-quarters of the way toward, and which resolves blocker #1
outright.

In the Plan 9 draw protocol, a client does not ship pixels. It sends **compact
drawing operations** — `draw`, `line`, `string`, allocate-image, free-image —
that reference **server-side images**. The pixels live in the server; the wire
carries verbs. That fits a 768-byte message ABI almost perfectly, and it is
*already the pattern in the tree*: `cond` looks each character up in its own
font and sends **8-byte glyph bitmaps** to a dumb kernel-side blitter. The
console server is a tiny draw server. The generalization is the same idea with
a richer verb set.

The shape:

- A **`drawd`** server owning the framebuffer, in exactly the mould of
  `cond`/`fsd`/`netd`, reusing the cluster-Phase-0 unified verb set and the
  per-task namespace machinery.
- Resources as files, as the resources arc already established:
  **`/dev/draw`**, **`/dev/mouse`**, **`/dev/kbd`**, **`/dev/window`**.
- **Image upload** (icons, glyph atlases, photos) — the one genuinely bulk
  operation — **chunks**, exactly like the `NETOP_RUN_MORE` pull loop built for
  `cpu` output streaming. That problem has been solved once here already.
- "SDL" becomes a thin client library over those files. "GTK" becomes a widget
  library over that.

Both are **pure userland**, both incremental, and **neither needs a kernel
change after `drawd` exists**. Path-based verbs paying off for a stateful
resource is the same result the dial-out arc got.

## Porting the real GTK/SDL is *harder* than writing our own

Worth stating explicitly, because the instinct runs the other way:

- **Real SDL** wants mmap'd surfaces, pthreads, `dlopen`, and a POSIX I/O
  surface far past what `libc/include` covers today.
- **Real GTK** drags in GLib, D-Bus, fontconfig, FreeType, Cairo, Pango and
  HarfBuzz — well over a million lines of C, on a libc personality that would
  need to be deep enough to host all of it.

For a message-passing microkernel with a 768-byte inline ABI and no shared
memory, **native-shaped equivalents are cheaper by a wide margin.** This is the
same conclusion the POSIX-divergence postmortem reached from the other
direction: the ABI is what it is, and the win comes from writing to its grain
rather than emulating someone else's.

## Rough sequencing and size

| Step | Size |
|---|---|
| USB boot-protocol **mouse** driver | days — reuses the xHCI HID path |
| **`drawd`** + `/dev/draw` ops protocol + double-buffering | the real arc, weeks |
| `/dev/mouse` + `/dev/window`, input routing & focus | medium |
| **SDL-shaped client lib** (surface, blit, events, timer) | small, once `drawd` exists |
| Font rasterizer, or multi-size bitmap fonts | medium |
| **Widget toolkit** (button/label/field/list/menu/layout) | medium-large, incremental |
| virtio-gpu (mode-setting, accelerated blit) | large — roadmap item **f** |
| Audio (`SDL_Audio`) | separate hardware arc; nothing exists |
| OpenGL | out of reach |

## The consumer question, unchanged

Roadmap item **f**'s honest caveat still stands: **nothing needs this yet.**
The framebuffer console suffices, and the terminal/editor work (roadmap items
1 and c) lives happily on the plain framebuffer — the VT100/cursor-addressing
work in `cond` is a nearer, better-motivated use of the same hours.

So the real question isn't difficulty, it's *want*: graphical applications for
their own sake. Which — for a project whose compiler goal is explicitly
"because it's the Ouroboros thing to do" — is a perfectly good reason. When it
comes, **the mouse driver and `drawd` are the two steps that unlock everything
else**, and the pixel-transfer model is the decision to get right on day one.
