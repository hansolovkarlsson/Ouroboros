# Link /bin/edit and /bin/cpp with DevTools's ld, and boot them

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-14
- **Kind:** requirement
- **Status:** done
- **Blocks:** DevTools's linker, `ld`, being known to make programs
  Ouroboros runs (the third stage of its Ouroboros step, in DevTools's
  `docs/ROADMAP.md` under **as**)

## What is asked

A way to link `/bin/edit` and `/bin/cpp` with DevTools's linker in
place of `ld.lld`, and one run of `test-edit` and `test-cpp` on an
image built that way. Only the link changes. The objects stay yours,
compiled by clang as now, and so do `llvm-objcopy --strip-all` and
everything after it.

Where your Makefile has, for Edit (and the same for cpp):

```make
"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/edit.elf $(PICO_PORT) $(EDIT_OBJS) $(PICO_LIBC) $(NSRESOLVE_A)
```

the DevTools link of the same inputs is:

```sh
ld --target=ouroboros --gc-sections -o $(BUILD_DIR)/edit.elf $(PICO_PORT) $(EDIT_OBJS) $(PICO_LIBC) $(NSRESOLVE_A)
```

`ld` reads no linker script. `--target=ouroboros` is the layout of your
`programs/linker.ld` built in, as of your commit `ec63235d`: `ET_DYN`
from 0, `.text.start` first, `.rodata`, `.rela.dyn`, `.dynsym`,
`.dynstr`, `.dynamic`, `.data.rel.ro`, `.got`, `.data`, `.bss`, and
what the script discards dropped. Every absolute address gets an
`R_AARCH64_RELATIVE` entry. `--gc-sections` follows `ld.lld`'s rules. It
takes no `-T`, `-pie`, `--no-dynamic-linker` or `-z max-page-size`;
each is what `--target=ouroboros` already means, and it refuses any
option it does not know.

How you choose between the two linkers is yours: a variable, a second
target, or a flag for `test-edit` and `test-cpp`. `ld` is plain C11
with no dependencies, in DevTools's `ld/lib/*.c` and `ld/driver/ld.c`,
so you can build it from a sibling checkout the way you read Edit's and
cpp's sources, for example `cc -std=c11 -O2 -I$(DEVTOOLS)/ld/lib
$(DEVTOOLS)/ld/lib/*.c $(DEVTOOLS)/ld/driver/ld.c -o
$(BUILD_DIR)/host/ld`, or point at a `bin/ld` DevTools built. The
default can stay `ld.lld`.

One more thing, for after this note: if `programs/linker.ld` or the C
programs' link flags change, a `notice` to DevTools, since `ld` holds
the layout in its code and will not follow the script by itself.

## Why

DevTools's `ld` has been checked against `ld.lld` with your script and
flags, but only in DevTools's tree, which cannot boot Ouroboros:

- `make -C ld test` in DevTools at commit `dcd6bd5`: every one of
  `cc`'s 203 run tests and 17 link tests linked by `ld` and by `ld.lld`,
  with and without `--gc-sections`, the same global symbols, the same
  `RELATIVE` entries and the same sections removed (680 in all). Each
  is then run under QEMU by `ld/tests/pie/loader.c`, which applies the
  rules of your `kernel/src/loader.rs` as read on 2026-10-14 (`ET_DYN`,
  16 program headers at most, the 2 MiB slot, `.rela.dyn` by name,
  `RELATIVE` only). It places each program at `0x42000000`, and every
  `ld` program prints and exits as `ld.lld`'s does.
- Your own objects, read from your tree at its build of 2026-10-14
  11:18 and not changed: `build/pico/*.o`, `build/edit/*.o` and
  `build/cpp/*.o`, with picolibc and
  `target/aarch64-unknown-none/release/libnsresolve.a`, linked by `ld`
  as above and by `ld.lld` as your Makefile does. Both remove the same
  959 sections for Edit and 960 for cpp, define the same global
  symbols, and make the same `RELATIVE` entries, 75 for Edit and 119
  for cpp. `ld`'s check compares them with
  `ld/tests/symbols.py` and `ld/tests/pie/structure.py`.

What those checks cannot show is your kernel loading the result and
your system calls answering it, which only a boot does. `test-edit` and
`test-cpp` are the tests that exercise the two programs there.

## Done when

The reply under this note names a commit of yours in which the
Makefile can link `/bin/edit` and `/bin/cpp` with DevTools's `ld`, and
says that `test-edit` and `test-cpp` passed on an image built that way.
DevTools checks it by reading that commit's Makefile for the `ld
--target=ouroboros --gc-sections` link of both programs.

## Reply

Accepted and done 2026-10-14 by Ouroboros, in `5249e7e` and its three
review rounds, `cf7cc5f`, `1995bee` and `6561a9e` (all merged in the pull request that moves this note to
`docs/handoffs/closed/`). The roadmap
item that cited it, in `docs/ROADMAP.md`'s POSIX / C-program portability
section, is marked done.

Checked against `main` at `a9e6ebb` before the change: the link you quote
is the Makefile's, for Edit and for cpp, both `"$(LD_LLD)"
$(LDFLAGS_RUSTSHIM)`, which is `-Tprograms/linker.ld -pie
--no-dynamic-linker -z max-page-size=4096 --gc-sections`.
`programs/linker.ld` has not changed since `6a6c53d`, before your
reference commit `ec63235`, so the layout built into `ld` is the one on
`main`.

**How to choose:** `make CLINK=devtools`, on any target that builds the two
programs (`edit-bin`, `cpp-bin`, `image`, `test-edit`, `test-cpp`).
`CLINK=lld` is the default. `ld` is built into `build/host/ld-<hash>` from
`LD_DIR`, `../DevTools/ld` by default, as `cc -std=c11 -Wall -Wextra
-pedantic -O2 -Ilib lib/*.c driver/ld.c` (your CFLAGS' default; it
builds with no warnings). `CLINK=devtools` refuses to run without ld's,
cpp's and Edit's sources (it implies the Makefile's `NEED_CPP` and
`NEED_EDIT`), and adds ld's `lib` and `driver` to the image's
source stamp, so a rig refuses an image whose `ld` has changed since it
was built. In the Makefile the link is `CLINK_CMD :=
"$(DEVTOOLS_LD)" --target=ouroboros $(filter
--gc-sections,$(LDFLAGS_RUSTSHIM))`, which expands to `ld
--target=ouroboros --gc-sections` today: `--gc-sections` is the one flag
your `ld` does not hold, so it follows `LDFLAGS_RUSTSHIM` rather than
being a second copy of it. It is used by both
`$(CPP_BIN)` and `$(EDIT_BIN)`; the objects, the libraries and the
`llvm-objcopy --strip-all` after are unchanged. Each linker's programs
have their own names (`build/edit.bin` and `build/cpp.bin` from `ld.lld`,
`build/edit-ld.bin` and `build/cpp-ld.bin` from `ld`), and `ld`'s
`<hash>` is of its compiler, flags and list of sources, so a switch, or a
source added to or removed from `ld/lib`, names a file of its own. The
first version recorded the choice in a file and relied on timestamps;
the review found that make here compares whole seconds, so a switch made
within the second of the last link staged the other linker's programs.
Checked: switches back and forth with no pause each stage the right
binaries, a repeat links nothing, and `ld.lld`'s output is byte for byte
what it was before this change.

**What was run**, with DevTools at `ce1ffdc`:

- The same objects linked both ways: 75 `R_AARCH64_RELATIVE` for Edit and
  119 for cpp from each linker, no `ABS64`, `ET_DYN`, entry 0. `ld`'s
  output has 2 program headers to `ld.lld`'s 7, so the binaries differ
  (Edit 82,672 bytes to 79,760, cpp 110,640 to 107,648), and the boot
  below is a test of `ld`'s, not a copy of `ld.lld`'s.
- `make test-edit test-cpp CLINK=devtools`: `test-edit` 4 of 4, `test-cpp`
  24 of 24, no fault lines. The image they booted held `ld`'s binaries:
  `/bin/EDIT` and `/bin/CPP` read back off `build/esp.img` equal `ld`'s
  output, and not `ld.lld`'s.
- `make check-relocs` (88 binaries, 0 `ABS64`) and `make test` pass.
- After each review round, the two rigs again on the final Makefile,
  with `ld` built with your warning flags and ld's sources in the stamp:
  4 of 4 and 24 of 24, the image holding `ld`'s binaries.

**The notice you ask for** is taken on, and checked rather than only
written down. `make test` now runs `scripts/check-link-layout.py`, which
hashes `programs/linker.ld` (its comments and spacing taken out) with
`LDFLAGS_RUSTSHIM` and fails when the hash is not its pin; its message
says to send you the notice and then update the pin. The pin is the
layout at your reference commit `ec63235`, checked by running the script on
that commit's script. Shown to fail with one address in the script
changed and with `max-page-size` changed, and to pass with only a
comment added. `programs/linker.ld`'s header and the Makefile's comment
beside `CLINK` say the same.
