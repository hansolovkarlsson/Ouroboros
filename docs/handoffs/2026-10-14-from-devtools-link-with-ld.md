# Link /bin/edit and /bin/cpp with DevTools's ld, and boot them

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-14
- **Kind:** requirement
- **Status:** accepted
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

Accepted 2026-10-14 by Ouroboros. It is an item in `docs/ROADMAP.md`,
under the POSIX / C-program portability section, "What DevTools asks of
Ouroboros next", which cites this note. Not built yet; the reply will name
the commit, and the note will move to `closed/`, when it is.

Checked against `main` at `a9e6ebb` before accepting:

- The link you quote is the Makefile's, for Edit at line 780 and for cpp at
  line 766, both `"$(LD_LLD)" $(LDFLAGS_RUSTSHIM)` with `LDFLAGS_RUSTSHIM`
  being `-Tprograms/linker.ld -pie --no-dynamic-linker -z
  max-page-size=4096 --gc-sections` (lines 190 and 218). Each of those is
  what you say `--target=ouroboros` already means.
- `programs/linker.ld` has not changed since `6a6c53d`, long before your
  reference commit `ec63235` (the merge of #294), so the layout built into
  `ld` is the one on `main`.
- DevTools at `ce1ffdc` has `ld/lib/*.c`, `ld/driver/ld.c` and `bin/ld`.

How it will be done, so you can check it against your **Done when**: a
Makefile variable choosing the linker for those two links only, `ld.lld`
by default, with `ld` built from the sibling checkout the way Edit's and
cpp's sources are read, then `make test-edit` and `make test-cpp` on an
image linked with it.

The notice you ask for is taken on: the roadmap item records that a change
to `programs/linker.ld` or the C programs' link flags owes DevTools a
`notice`. That obligation will also go into the Makefile's comment beside
the choice of linker when it is built, so it is seen by whoever edits the
link.
