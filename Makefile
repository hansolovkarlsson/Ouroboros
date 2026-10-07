TARGET       := aarch64-unknown-uefi
USER_TARGET  := aarch64-unknown-none
PROFILE      ?= debug
KERNEL       := target/$(TARGET)/$(PROFILE)/BOOTAA64.efi
# Userland programs (shell/) always build in release profile, regardless
# of $(PROFILE) above - a hard requirement, not a style choice, since the
# relocating loader work switched to position-independent linking
# (.cargo/config.toml). Confirmed by direct experiment: a *debug* build
# fails to link at all under that model - rust-lld rejects an
# R_AARCH64_ABS64 relocation inside `core::fmt::builders::PadAdapter`,
# pulled in from the prebuilt (not rebuilt-per-project) libcore.rlib by
# ordinary debug-build panic/bounds-check formatting machinery, which
# that prebuilt rlib was never compiled with -C relocation-model=pic
# support for. A *release* build's optimizer eliminates enough of that
# unreachable-in-practice panic-formatting code that the poisoned object
# code never gets pulled into the link at all. See loader.rs's module
# doc comment and docs/processes.md's "Binary format" section for the
# full writeup - this is a real, hard-won toolchain constraint, not a
# guess, and not something -Z build-std would be worth reintroducing a
# nightly-toolchain dependency to work around (see rust-toolchain.toml's
# own "no nightly, no -Z build-std needed" comment).
SHELL_ELF    := target/$(USER_TARGET)/release/shell
SHELL_BIN    := target/$(USER_TARGET)/release/shell.bin
HELLO_ELF    := target/$(USER_TARGET)/release/hello
HELLO_BIN    := target/$(USER_TARGET)/release/hello.bin
PONG_ELF     := target/$(USER_TARGET)/release/pong
PONG_BIN     := target/$(USER_TARGET)/release/pong.bin
FSD_ELF      := target/$(USER_TARGET)/release/fsd
FSD_BIN      := target/$(USER_TARGET)/release/fsd.bin
UPPER_ELF    := target/$(USER_TARGET)/release/upper
UPPER_BIN    := target/$(USER_TARGET)/release/upper.bin
COND_ELF     := target/$(USER_TARGET)/release/cond
COND_BIN     := target/$(USER_TARGET)/release/cond.bin
NETD_ELF     := target/$(USER_TARGET)/release/netd
NETD_BIN     := target/$(USER_TARGET)/release/netd.bin
ACCTD_ELF    := target/$(USER_TARGET)/release/accountd
ACCTD_BIN    := target/$(USER_TARGET)/release/accountd.bin
ARGS_ELF     := target/$(USER_TARGET)/release/args
ARGS_BIN     := target/$(USER_TARGET)/release/args.bin
ECHO_ELF     := target/$(USER_TARGET)/release/echo
ECHO_BIN     := target/$(USER_TARGET)/release/echo.bin
UPTIME_ELF   := target/$(USER_TARGET)/release/uptime
UPTIME_BIN   := target/$(USER_TARGET)/release/uptime.bin
CLEAR_ELF    := target/$(USER_TARGET)/release/clear
CLEAR_BIN    := target/$(USER_TARGET)/release/clear.bin
LS_ELF       := target/$(USER_TARGET)/release/ls
LS_BIN       := target/$(USER_TARGET)/release/ls.bin
CAT_ELF      := target/$(USER_TARGET)/release/cat
CAT_BIN      := target/$(USER_TARGET)/release/cat.bin
MKDIR_ELF    := target/$(USER_TARGET)/release/mkdir
MKDIR_BIN    := target/$(USER_TARGET)/release/mkdir.bin
RMDIR_ELF    := target/$(USER_TARGET)/release/rmdir
RMDIR_BIN    := target/$(USER_TARGET)/release/rmdir.bin
TOUCH_ELF    := target/$(USER_TARGET)/release/touch
TOUCH_BIN    := target/$(USER_TARGET)/release/touch.bin
RM_ELF       := target/$(USER_TARGET)/release/rm
RM_BIN       := target/$(USER_TARGET)/release/rm.bin
CP_ELF       := target/$(USER_TARGET)/release/cp
CP_BIN       := target/$(USER_TARGET)/release/cp.bin
MV_ELF       := target/$(USER_TARGET)/release/mv
MV_BIN       := target/$(USER_TARGET)/release/mv.bin
WRITEAT_ELF  := target/$(USER_TARGET)/release/writeat
WRITEAT_BIN  := target/$(USER_TARGET)/release/writeat.bin
CHMOD_ELF    := target/$(USER_TARGET)/release/chmod
CHMOD_BIN    := target/$(USER_TARGET)/release/chmod.bin
CHOWN_ELF    := target/$(USER_TARGET)/release/chown
CHOWN_BIN    := target/$(USER_TARGET)/release/chown.bin
TREE_ELF     := target/$(USER_TARGET)/release/tree
TREE_BIN     := target/$(USER_TARGET)/release/tree.bin
PWD_ELF      := target/$(USER_TARGET)/release/pwd
PWD_BIN      := target/$(USER_TARGET)/release/pwd.bin
PRINTENV_ELF := target/$(USER_TARGET)/release/printenv
PRINTENV_BIN := target/$(USER_TARGET)/release/printenv.bin
RDPROBE_ELF  := target/$(USER_TARGET)/release/rdprobe
RDPROBE_BIN  := target/$(USER_TARGET)/release/rdprobe.bin
FPPROBE_ELF  := target/$(USER_TARGET)/release/fpprobe
FPPROBE_BIN  := target/$(USER_TARGET)/release/fpprobe.bin
VTPROBE_ELF  := target/$(USER_TARGET)/release/vtprobe
VTPROBE_BIN  := target/$(USER_TARGET)/release/vtprobe.bin
ID_ELF       := target/$(USER_TARGET)/release/id
ID_BIN       := target/$(USER_TARGET)/release/id.bin
PASSWD_ELF   := target/$(USER_TARGET)/release/passwd
PASSWD_BIN   := target/$(USER_TARGET)/release/passwd.bin
USERADD_ELF  := target/$(USER_TARGET)/release/useradd
USERADD_BIN  := target/$(USER_TARGET)/release/useradd.bin
GROUPADD_ELF := target/$(USER_TARGET)/release/groupadd
GROUPADD_BIN := target/$(USER_TARGET)/release/groupadd.bin
CLUSTERKEY_ELF := target/$(USER_TARGET)/release/clusterkey
CLUSTERKEY_BIN := target/$(USER_TARGET)/release/clusterkey.bin
USERMOD_ELF  := target/$(USER_TARGET)/release/usermod
USERMOD_BIN  := target/$(USER_TARGET)/release/usermod.bin
WRITE_ELF    := target/$(USER_TARGET)/release/write
WRITE_BIN    := target/$(USER_TARGET)/release/write.bin
READKEY_ELF  := target/$(USER_TARGET)/release/readkey
READKEY_BIN  := target/$(USER_TARGET)/release/readkey.bin
MORE_ELF     := target/$(USER_TARGET)/release/more
MORE_BIN     := target/$(USER_TARGET)/release/more.bin
SEND_ELF     := target/$(USER_TARGET)/release/send
SEND_BIN     := target/$(USER_TARGET)/release/send.bin
RECV_ELF     := target/$(USER_TARGET)/release/recv
RECV_BIN     := target/$(USER_TARGET)/release/recv.bin
EDTEST_ELF   := target/$(USER_TARGET)/release/edtest
EDTEST_BIN   := target/$(USER_TARGET)/release/edtest.bin
KEYPROBE_ELF := target/$(USER_TARGET)/release/keyprobe
KEYPROBE_BIN := target/$(USER_TARGET)/release/keyprobe.bin
SELFTEST_ELF := target/$(USER_TARGET)/release/selftest
SELFTEST_BIN := target/$(USER_TARGET)/release/selftest.bin
BOOTID_ELF   := target/$(USER_TARGET)/release/bootid
BOOTID_BIN   := target/$(USER_TARGET)/release/bootid.bin
MAN_ELF      := target/$(USER_TARGET)/release/man
MAN_BIN      := target/$(USER_TARGET)/release/man.bin
PING_ELF     := target/$(USER_TARGET)/release/ping
PING_BIN     := target/$(USER_TARGET)/release/ping.bin
WC_ELF       := target/$(USER_TARGET)/release/wc
WC_BIN       := target/$(USER_TARGET)/release/wc.bin
GREP_ELF     := target/$(USER_TARGET)/release/grep
GREP_BIN     := target/$(USER_TARGET)/release/grep.bin
HEAD_ELF     := target/$(USER_TARGET)/release/head
HEAD_BIN     := target/$(USER_TARGET)/release/head.bin
TAIL_ELF     := target/$(USER_TARGET)/release/tail
TAIL_BIN     := target/$(USER_TARGET)/release/tail.bin
NL_ELF       := target/$(USER_TARGET)/release/nl
NL_BIN       := target/$(USER_TARGET)/release/nl.bin
REV_ELF      := target/$(USER_TARGET)/release/rev
REV_BIN      := target/$(USER_TARGET)/release/rev.bin
UNIQ_ELF     := target/$(USER_TARGET)/release/uniq
UNIQ_BIN     := target/$(USER_TARGET)/release/uniq.bin
SORT_ELF     := target/$(USER_TARGET)/release/sort
SORT_BIN     := target/$(USER_TARGET)/release/sort.bin
RESOLVE_ELF  := target/$(USER_TARGET)/release/resolve
RESOLVE_BIN  := target/$(USER_TARGET)/release/resolve.bin
FETCH_ELF    := target/$(USER_TARGET)/release/fetch
FETCH_BIN    := target/$(USER_TARGET)/release/fetch.bin
DIAL_ELF     := target/$(USER_TARGET)/release/dial
DIAL_BIN     := target/$(USER_TARGET)/release/dial.bin
SERVE_ELF    := target/$(USER_TARGET)/release/serve
SERVE_BIN    := target/$(USER_TARGET)/release/serve.bin
# All generated artifacts land under $(BUILD_DIR) so the repo root stays
# source-only. The cargo `target/` dir is separate (cargo owns it). Every
# path below is derived from BUILD_DIR, so pointing it elsewhere moves the
# whole lot. $(BUILD_DIR) is created lazily by the `esp` staging step
# (mkdir -p $(ESP_DIR)/...), which every image target depends on.
BUILD_DIR    := build
ESP_DIR      := $(BUILD_DIR)/esp
ESP_IMG      := $(ESP_DIR).img
ESP_HDD      := $(ESP_DIR).hdd
# WHICH MACHINE an image is built as. Per-machine keypairs mean an image carries
# an identity, so the two-node rig cannot boot one image twice any more: node A's
# disk and node B's disk hold different private keys and the same `authorized`
# file. Override to build a differently-identified disk:
#     make image-ext2 CLUSTER_NODE=node-b
CLUSTER_NODE ?= node-a
GPT_IMG      := $(BUILD_DIR)/espgpt.img
EXFAT_IMG    := $(BUILD_DIR)/espexfat.img
EXFAT_PART   := $(BUILD_DIR)/exfatpart.img
EXT2_IMG     := $(BUILD_DIR)/espext2.img
EXT2_PART    := $(BUILD_DIR)/ext2part.img
USBSTICK_IMG := $(BUILD_DIR)/usbstick.img
STALL_IMG := $(BUILD_DIR)/usb-hub-stall.img
NET_PCAP     := $(BUILD_DIR)/net.pcap
# mke2fs (ext2 image builder) from Homebrew's keg-only e2fsprogs - macOS has no
# native ext2 tooling. `brew install e2fsprogs` provides it. Used only by the
# ext2part.img target (fsd/src/ext2.rs testing).
MKE2FS       := $(shell brew --prefix e2fsprogs 2>/dev/null)/sbin/mke2fs
DEBUGFS      := $(shell brew --prefix e2fsprogs 2>/dev/null)/sbin/debugfs
OVMF         := $(shell brew --prefix qemu 2>/dev/null)/share/qemu/edk2-aarch64-code.fd
PDT          := /Applications/Parallels Desktop.app/Contents/MacOS/prl_disk_tool

# llvm-objcopy ships with the `llvm-tools` rustup component (added via
# rust-toolchain.toml), but unlike cargo/rustc itself it isn't on PATH or
# reachable via `rustup which` - only cargo-binutils' `cargo objcopy`
# subcommand knows to find it that way, and pulling in cargo-binutils for
# one call felt like more dependency than this needs. Its real location is
# a fixed, discoverable path under the toolchain's own sysroot instead.
HOST_TRIPLE  := $(shell rustc -vV | sed -n 's/^host: //p')
OBJCOPY      := $(shell rustc --print sysroot)/lib/rustlib/$(HOST_TRIPLE)/bin/llvm-objcopy
# C toolchain for the userland-libc arc: clang (aarch64-none ELF codegen) and
# Rust's bundled LLD as the ELF linker - so a C program builds into the same
# self-relocating PIE format the Rust programs use (programs/linker.ld), with no
# extra cross-toolchain to install. See libc/ and docs/processes.md.
CC           := clang
LD_LLD       := $(shell rustc --print sysroot)/lib/rustlib/$(HOST_TRIPLE)/bin/gcc-ld/ld.lld
CFLAGS_OS    := --target=aarch64-unknown-none -ffreestanding -fPIC -fno-stack-protector -O2 -Wall
LDFLAGS_OS   := -Tprograms/linker.ld -pie --no-dynamic-linker -z max-page-size=4096
CHELLO_BIN   := $(BUILD_DIR)/chello.bin
# The minimal libc (libc/src/*.c -> objects a C program links against, plus the
# public headers in libc/include). Compiled with -fno-builtin so the optimizer
# doesn't rewrite string.c's memcpy/memset loops into calls to themselves.
LIBC_SRCS    := $(wildcard libc/src/*.c)
LIBC_OBJS    := $(patsubst libc/src/%.c,$(BUILD_DIR)/libc/%.o,$(LIBC_SRCS))
LIBC_CFLAGS  := $(CFLAGS_OS) -Ilibc/include -fno-builtin
CDEMO_BIN    := $(BUILD_DIR)/cdemo.bin
CFILE_BIN    := $(BUILD_DIR)/cfile.bin
CLEAK_BIN    := $(BUILD_DIR)/cleak.bin
CFIDHOLD_BIN := $(BUILD_DIR)/cfidhold.bin
NSDEMO_BIN   := $(BUILD_DIR)/nsdemo.bin
CREMOTE_BIN  := $(BUILD_DIR)/cremote.bin
CBIG_BIN     := $(BUILD_DIR)/cbig.bin
CWRITE_BIN   := $(BUILD_DIR)/cwrite.bin
# The Rust namespace-resolution shim a C program links to reach
# `ninep_abi::resolve_ns` (docs/roadmap/roadmap-fid-verbs.md step 3). A STATICLIB, since
# the consumer is a clang+LLD link, not a Rust one.
NSRESOLVE_A  := target/aarch64-unknown-none/release/libnsresolve.a
# --gc-sections IS REQUIRED, not a size optimization. Without it this link fails
# outright: `rust-lld: error: relocation R_AARCH64_ABS64 cannot be used against
# local symbol`, from the PREBUILT `core` bundled into the staticlib - the same
# wall that makes alloc's collections unlinkable here, one crate down. The
# offending .rodata is UNREFERENCED, so collecting it removes the relocation
# rather than hiding it (verified: 0 ABS64, 7 RELATIVE afterwards, and the entry
# point stays at 0x0 because the linker script KEEPs .text.start).
# NOT with LLD's -O2, which reintroduces the failure - do not add it.
LDFLAGS_RUSTSHIM := $(LDFLAGS_OS) --gc-sections
# EVERY C program links the shim now, not just the demo: libc/src/file.c
# resolves each path through the task namespace, so `ouro_ns_resolve` is a hard
# dependency of open()/read()/write(). That is the cost of the C arc reaching a
# remote mount at all, and it is paid by all of them - which also means
# --gc-sections is now required for the whole C arc, not one target.
# The picolibc port (the real C library): a prebuilt static libc.a + headers
# under third_party/picolibc-prebuilt (built once from picolibc 1.8.9 by
# scripts/build-picolibc.sh; committed so `make` needs no meson/ninja). A C
# program links picolibc's libc.a with OUR porting layer - crt0/os/file compiled
# against picolibc's headers (so struct stat etc. match) plus libc/pico/builtins.c
# (the two compiler-rt 128-bit shifts picolibc's float printf needs). picolibc is
# built -fPIC, so it self-relocates (R_AARCH64_RELATIVE only) under our loader.
PICO_DIR     := third_party/picolibc-prebuilt
# picolibc's headers, and the one POSIX feature they hide here that the port
# has: picolibc's features.h defines _POSIX_MONOTONIC_CLOCK for RTEMS only, so
# without this <time.h> names no CLOCK_MONOTONIC, which libc/pico/clock.c
# answers (step 5 of the C-hosting plan). On every picolibc compile line.
PICO_INC     := -I$(PICO_DIR)/include -D_POSIX_MONOTONIC_CLOCK=200112L
# clang's own headers, the freestanding ones CFLAGS_OS compiles against, are
# the second system directory the image stages (step 6 of the C-hosting plan).
# Their directory is asked of clang once, inside the esp recipe, so no other
# make runs clang for it and a failure there stops the stage with a message.
# The eleven by name, and the helpers they include by glob, so a toolchain
# update that adds a helper is staged without a kept list going stale; a
# family the toolchain does not have (older clangs have no __float_*.h) is
# skipped rather than failing the stage.
CLANG_HDRS   := stddef.h stdarg.h stdbool.h float.h limits.h stdatomic.h stdint.h \
                inttypes.h stdnoreturn.h stdalign.h iso646.h
CLANG_GLOBS  := __stddef_*.h __stdarg_*.h __float_*.h
PICO_LIBC    := $(PICO_DIR)/lib/libc.a
PICO_PORT    := $(BUILD_DIR)/pico/crt0.o $(BUILD_DIR)/pico/os.o $(BUILD_DIR)/pico/file.o $(BUILD_DIR)/pico/builtins.o $(BUILD_DIR)/pico/clock.o
CPICO_BIN    := $(BUILD_DIR)/cpico.bin
CMEM_BIN     := $(BUILD_DIR)/cmem.bin
CRENAME_BIN  := $(BUILD_DIR)/crename.bin
CFSTAT_BIN   := $(BUILD_DIR)/cfstat.bin
CARGS_BIN    := $(BUILD_DIR)/cargs.bin
CARGSH_BIN   := $(BUILD_DIR)/cargsh.bin
CERRNO_BIN   := $(BUILD_DIR)/cerrno.bin
CENV_BIN     := $(BUILD_DIR)/cenv.bin
CENVH_BIN    := $(BUILD_DIR)/cenvh.bin
CCLOCK_BIN   := $(BUILD_DIR)/cclock.bin
# DevTools's C preprocessor, built here for Ouroboros (step 8 of the C-hosting
# plan): its sources are read from CPP_DIR, a sibling checkout by default.
# CPP_DIR must not contain spaces: its files are make prerequisites.
CPP_DIR      ?= ../DevTools/cpp
CPP_BIN      := $(BUILD_DIR)/cpp.bin
HAVE_CPP     := $(wildcard $(CPP_DIR)/driver/cpp.c)
CPP_OBJS     := $(if $(HAVE_CPP),$(patsubst $(CPP_DIR)/lib/%.c,$(BUILD_DIR)/cpp/%.o,$(wildcard $(CPP_DIR)/lib/*.c)) $(BUILD_DIR)/cpp/cpp.o)
# Whether an image without /bin/cpp is acceptable: `optional` (the default,
# for development, so this tree alone builds) or `required`, which stops at
# once when CPP_DIR has no sources. A release passes NEED_CPP=required, and
# cpp-bin and test-cpp imply it, with `override` so no command-line value can
# switch it off: one switch rather than a guard in each (the reviews of
# #226). Not named CPP: make predefines that as the C preprocessor command,
# so `CPP ?=` never took and `CPP=required` would reach every tool that runs
# $CPP. `make clean` is never stopped by it.
NEED_CPP     ?= optional
ifneq ($(filter cpp-bin test-cpp,$(MAKECMDGOALS)),)
override NEED_CPP := required
endif
ifeq ($(NEED_CPP),required)
ifeq ($(HAVE_CPP),)
ifneq ($(filter-out clean,$(or $(MAKECMDGOALS),all)),)
$(error NEED_CPP=required: no cpp sources at $(CPP_DIR) (set CPP_DIR))
endif
endif
endif

CARGO_FLAGS :=
ifeq ($(PROFILE),release)
CARGO_FLAGS += --release
endif

.PHONY: all build check-site shell-bin hello-bin pong-bin fsd-bin upper-bin cond-bin netd-bin accountd-bin args-bin echo-bin uptime-bin clear-bin ls-bin cat-bin mkdir-bin rmdir-bin touch-bin rm-bin cp-bin mv-bin writeat-bin chmod-bin chown-bin tree-bin pwd-bin printenv-bin rdprobe-bin fpprobe-bin vtprobe-bin id-bin passwd-bin useradd-bin groupadd-bin usermod-bin clusterkey-bin chello-bin cdemo-bin cfile-bin cleak-bin cfidhold-bin nsdemo-bin cremote-bin cbig-bin cwrite-bin cpico-bin cmem-bin crename-bin cfstat-bin cargs-bin cargsh-bin cerrno-bin cenv-bin cenvh-bin cclock-bin cpp-bin write-bin readkey-bin more-bin send-bin recv-bin selftest-bin bootid-bin edtest-bin keyprobe-bin man-bin ping-bin resolve-bin fetch-bin dial-bin serve-bin wc-bin grep-bin head-bin sort-bin esp run run-virtio-console run-usb-kbd run-usb-multi run-usb-hub run-gicv3 image run-image run-image-9p run-image-9p-client run-image-2vm-a run-image-2vm-b run-image-2vm-ext2-a run-image-2vm-ext2-b image-gpt run-image-gpt image-exfat run-image-exfat image-ext2 run-image-ext2 images-2vm images-2vm-ext2 parallels-hdd sdcard stick release test check-relocs check-xhci-barriers test-parallels test-keyboard-chain test-usb-hub image-stall test-early-fault run-el2 test-el1-drop test-reentrant-session test-async-rmount test-held-keys test-heap test-fpsimd test-cond-vt test-nav-keys test-cargs test-cerrno test-cenv test-cclock test-include test-cpp test-unmount test-crename clean

# Overridable by `make test-parallels VM_NAME=... CMDS=... BOOT_WAIT=...`.
VM_NAME     ?= Ouroboros
CMDS        ?= help
BOOT_WAIT   ?= 12

# The conventional entry point, and the default goal because it is the first
# target in the file. `all` builds the kernel only - the same thing a bare
# `make` has always done here - because every userland program needs a
# different --target and is built by its own *-bin target (see below); `esp`
# is what pulls the whole staging tree together.
all: build

build:
	cargo build $(CARGO_FLAGS)

# Built separately from `build`: a different target (aarch64-unknown-none,
# not the workspace-default aarch64-unknown-uefi - see Cargo.toml's
# default-members comment), always --release (see SHELL_ELF's own comment
# above for why), and staged as a real, if stripped, ELF file - not
# `objcopy -O binary` anymore. `loader.rs` is a real (if narrowly-scoped)
# ELF loader now: it needs the program header table and a `.rela.dyn`
# section to actually load and relocate this correctly, both of which a
# raw-binary conversion would have thrown away. `--strip-all` alone
# (dropping `-O binary`) still shrinks the file by removing the symbol
# table and debug info - confirmed by direct comparison to still leave
# every section the loader actually reads (program headers, section
# headers, `.rela.dyn`'s contents) fully intact.
shell-bin:
	cargo build -p shell --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(SHELL_ELF) $(SHELL_BIN)

# Same recipe as shell-bin for the second userland program (hello/) -
# same target, same release-only constraint, same shared linker script
# (the -T flag in .cargo/config.toml is workspace-relative).
hello-bin:
	cargo build -p hello --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(HELLO_ELF) $(HELLO_BIN)

pong-bin:
	cargo build -p pong --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PONG_ELF) $(PONG_BIN)

# The filesystem server (fsd/) - boot-loaded by the kernel from
# \EFI\ORBS\FSD.BIN into its reserved task slot 2, not spawned.
fsd-bin:
	cargo build -p fsd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(FSD_ELF) $(FSD_BIN)

upper-bin:
	cargo build -p upper --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(UPPER_ELF) $(UPPER_BIN)

# The console server (cond/) - boot-loaded by the kernel from
# \EFI\ORBS\COND.BIN into its reserved task slot 3, not spawned (same
# shape as the filesystem server in slot 2).
cond-bin:
	cargo build -p cond --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(COND_ELF) $(COND_BIN)

# The network server (netd/) - loaded into reserved task slot 4, same shape
# as fsd (slot 2) and cond (slot 3).
netd-bin:
	cargo build -p netd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(NETD_ELF) $(NETD_BIN)

# The account server (programs/servers/accountd) - the fifth protected server,
# task slot 5. Holds the policy for changing a password now that /etc/shadow is
# root-only, so a normal user's `passwd` has something privileged to ask.
accountd-bin:
	cargo build -p accountd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(ACCTD_ELF) $(ACCTD_BIN)

# The argv proof program (args/) - spawned via `exec`, prints its argument
# vector. Same recipe as every other userland program.
args-bin:
	cargo build -p args --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(ARGS_ELF) $(ARGS_BIN)

# Externalized commands (standalone-binaries Stage 4): former shell builtins,
# now real /bin programs sharing the `ulib` support crate. Same recipe as
# every other userland program.
echo-bin:
	cargo build -p echo --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(ECHO_ELF) $(ECHO_BIN)

uptime-bin:
	cargo build -p uptime --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(UPTIME_ELF) $(UPTIME_BIN)

clear-bin:
	cargo build -p clear --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CLEAR_ELF) $(CLEAR_BIN)

ls-bin:
	cargo build -p ls --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(LS_ELF) $(LS_BIN)

cat-bin:
	cargo build -p cat --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CAT_ELF) $(CAT_BIN)

mkdir-bin:
	cargo build -p mkdir --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(MKDIR_ELF) $(MKDIR_BIN)

rmdir-bin:
	cargo build -p rmdir --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RMDIR_ELF) $(RMDIR_BIN)

touch-bin:
	cargo build -p touch --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(TOUCH_ELF) $(TOUCH_BIN)

rm-bin:
	cargo build -p rm --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RM_ELF) $(RM_BIN)

cp-bin:
	cargo build -p cp --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CP_ELF) $(CP_BIN)

mv-bin:
	cargo build -p mv --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(MV_ELF) $(MV_BIN)

writeat-bin:
	cargo build -p writeat --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(WRITEAT_ELF) $(WRITEAT_BIN)

chmod-bin:
	cargo build -p chmod --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CHMOD_ELF) $(CHMOD_BIN)

chown-bin:
	cargo build -p chown --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CHOWN_ELF) $(CHOWN_BIN)

tree-bin:
	cargo build -p tree --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(TREE_ELF) $(TREE_BIN)

pwd-bin:
	cargo build -p pwd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PWD_ELF) $(PWD_BIN)

printenv-bin:
	cargo build -p printenv --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PRINTENV_ELF) $(PRINTENV_BIN)

# The check that the kernel's per-task store reads accept a buffer larger than
# the store (copy_out in kernel/src/syscall.rs); run by scripts/test-cenv.py.
rdprobe-bin:
	cargo build -p rdprobe --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RDPROBE_ELF) $(RDPROBE_BIN)

fpprobe-bin:
	cargo build -p fpprobe --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(FPPROBE_ELF) $(FPPROBE_BIN)

vtprobe-bin:
	cargo build -p vtprobe --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(VTPROBE_ELF) $(VTPROBE_BIN)

id-bin:
	cargo build -p id --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(ID_ELF) $(ID_BIN)

passwd-bin:
	cargo build -p passwd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PASSWD_ELF) $(PASSWD_BIN)

useradd-bin:
	cargo build -p useradd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(USERADD_ELF) $(USERADD_BIN)

groupadd-bin:
	cargo build -p groupadd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(GROUPADD_ELF) $(GROUPADD_BIN)

usermod-bin:
	cargo build -p usermod --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(USERMOD_ELF) $(USERMOD_BIN)

# On-device cluster identity (docs/roadmap/roadmap-cluster-keys.md step 6c).
clusterkey-bin:
	cargo build -p clusterkey --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CLUSTERKEY_ELF) $(CLUSTERKEY_BIN)

# The first C program (userland-libc arc): compile with clang, link with LLD to
# the shared PIE linker script, strip to the same .bin shape a Rust program has.
# Self-contained for now (libc/hello.c has its own _start + syscall stubs); the
# real libc grows from here. See docs/processes.md's C-program section.
chello-bin:
	mkdir -p $(BUILD_DIR)
	$(CC) $(CFLAGS_OS) -c libc/hello.c -o $(BUILD_DIR)/chello.o
	"$(LD_LLD)" $(LDFLAGS_OS) -o $(BUILD_DIR)/chello.elf $(BUILD_DIR)/chello.o
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/chello.elf $(CHELLO_BIN)

# Compile each minimal-libc source to an object (-fno-builtin, see LIBC_CFLAGS).
# The headers too: a change to sys.h (a wire constant) or sys/stat.h (a
# struct) must rebuild every object, or a stale one ships beside the new
# header with nothing to say so.
LIBC_HDRS    := $(wildcard libc/include/*.h libc/include/sys/*.h)
$(BUILD_DIR)/libc/%.o: libc/src/%.c $(LIBC_HDRS)
	mkdir -p $(BUILD_DIR)/libc
	$(CC) $(LIBC_CFLAGS) -c $< -o $@

# A C program using the minimal libc: compile against the headers, link with the
# libc objects (crt0 provides _start). The demo may use compiler builtins (a
# struct copy -> memcpy) since those resolve to the libc's own memcpy.
cdemo-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/demo.c -o $(BUILD_DIR)/cdemo.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cdemo.elf $(BUILD_DIR)/cdemo.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cdemo.elf $(CDEMO_BIN)

# A C program using the libc's file I/O (open/read/write/close/fstat over fsd).
cfile-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cfile.c -o $(BUILD_DIR)/cfile.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cfile.elf $(BUILD_DIR)/cfile.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cfile.elf $(CFILE_BIN)

# Holds a file open across `unmount` and `mount -a`: the check that a fid on an
# unmounted filesystem stays allocated, dead, until closed. Driven by
# scripts/test-unmount.py. See libc/cfidhold.c.
cfidhold-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cfidhold.c -o $(BUILD_DIR)/cfidhold.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cfidhold.elf $(BUILD_DIR)/cfidhold.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cfidhold.elf $(CFIDHOLD_BIN)

# A C program that LEAKS a fid on purpose (exits through the raw EXIT syscall,
# past _exit's close-all): the check for fsd's identity-keyed fid reaper. See
# libc/cleak.c and docs/testing/testing-qemu.md.
cleak-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cleak.c -o $(BUILD_DIR)/cleak.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cleak.elf $(BUILD_DIR)/cleak.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cleak.elf $(CLEAK_BIN)

$(NSRESOLVE_A): nsresolve/src/lib.rs nsresolve/Cargo.toml ninep-abi/src/lib.rs syscall-abi/src/lib.rs
	cargo build -p nsresolve --target aarch64-unknown-none --release

# A C program that resolves paths through its OWN namespace - which C could not
# do at all before, and is why a C open() of a remote mount never left the
# machine. The runnable half of step 3's build gate.
nsdemo-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/nsdemo.c -o $(BUILD_DIR)/nsdemo.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/nsdemo.elf $(BUILD_DIR)/nsdemo.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/nsdemo.elf $(NSDEMO_BIN)

# A C program that opens and reads a file on a REMOTE mount - the step-3b
# check. It reads a LOCAL path first, on purpose: every existing C program takes
# that route, so a change that fixed the remote case by breaking the local one
# would otherwise look like a pass.
cremote-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cremote.c -o $(BUILD_DIR)/cremote.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cremote.elf $(BUILD_DIR)/cremote.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cremote.elf $(CREMOTE_BIN)

# The multi-chunk remote-read witness for the client session (Decision 4). Same
# link shape as cremote (libc + the nsresolve shim).
cbig-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cbig.c -o $(BUILD_DIR)/cbig.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cbig.elf $(BUILD_DIR)/cbig.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cbig.elf $(CBIG_BIN)

# The remote-WRITE witness for NP_PWRITE (step 7). Same link shape as cbig.
cwrite-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cwrite.c -o $(BUILD_DIR)/cwrite.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cwrite.elf $(BUILD_DIR)/cwrite.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cwrite.elf $(CWRITE_BIN)

# The porting layer for picolibc: our syscall stubs (crt0/os/file) compiled
# against picolibc's headers so struct/ABI shapes match, plus the 128-bit-shift
# builtins. Kept apart from LIBC_OBJS (the hand-rolled libc) - a picolibc program
# does NOT link our stdio.c/stdlib.c/string.c (picolibc supplies those).
#
# libc/include is on the QUOTE path only (-iquote), so "sys.h" and
# "nsresolve.h" resolve and an angle-bracket include can never reach a
# hand-rolled header: the shapes are picolibc's whatever the flag order. Until
# 2026-10-05 it was -Ilibc/include, first, so <sys/stat.h> was the hand-rolled
# struct stat (st_size at 0, st_mode at 8, against picolibc's 16 and 4), and
# fstat wrote a picolibc program's size into st_dev/st_ino (found by
# libc/cfstat.c; the review of #216 asked for the wrong header to be
# unspellable rather than out-ordered).
PICO_PORT_CFLAGS := $(CFLAGS_OS) $(PICO_INC) -iquote libc/include -fno-builtin
$(BUILD_DIR)/pico/crt0.o: libc/src/crt0.c $(LIBC_HDRS)
	@mkdir -p $(BUILD_DIR)/pico
	$(CC) $(PICO_PORT_CFLAGS) -c $< -o $@
$(BUILD_DIR)/pico/os.o: libc/src/os.c $(LIBC_HDRS)
	@mkdir -p $(BUILD_DIR)/pico
	$(CC) $(PICO_PORT_CFLAGS) -c $< -o $@
$(BUILD_DIR)/pico/file.o: libc/src/file.c $(LIBC_HDRS)
	@mkdir -p $(BUILD_DIR)/pico
	$(CC) $(PICO_PORT_CFLAGS) -DOURO_HAVE_ERRNO -c $< -o $@
$(BUILD_DIR)/pico/builtins.o: libc/pico/builtins.c
	@mkdir -p $(BUILD_DIR)/pico
	$(CC) $(CFLAGS_OS) -fno-builtin -c $< -o $@
# gettimeofday and clock_gettime from MONOTONIC_US (step 5 of the C-hosting plan).
$(BUILD_DIR)/pico/clock.o: libc/pico/clock.c $(LIBC_HDRS)
	@mkdir -p $(BUILD_DIR)/pico
	$(CC) $(PICO_PORT_CFLAGS) -c $< -o $@

# A C program linked against the REAL picolibc (float printf, snprintf, qsort,
# strtol - what the hand-rolled libc couldn't do). Runs as /bin/CPICO.
cpico-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/picodemo.c -o $(BUILD_DIR)/pico/picodemo.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cpico.elf $(PICO_PORT) $(BUILD_DIR)/pico/picodemo.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cpico.elf $(CPICO_BIN)

# The heap a C program gets, held through picolibc's malloc and every byte
# checked: prints the heap's size beside what malloc held live. The check for
# the 1 MiB heap Proem asked for. Runs as /bin/CMEM.
cmem-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/cmem.c -o $(BUILD_DIR)/pico/cmem.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cmem.elf $(PICO_PORT) $(BUILD_DIR)/pico/cmem.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cmem.elf $(CMEM_BIN)

# unlink and rename through picolibc (`remove`, `rename`): the checks Proem's and
# Edit's handoff notes name. Driven by scripts/test-crename.py. Runs as
# /bin/CRENAME.
crename-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/crename.c -o $(BUILD_DIR)/pico/crename.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/crename.elf $(PICO_PORT) $(BUILD_DIR)/pico/crename.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/crename.elf $(CRENAME_BIN)

# fstat through picolibc: every field it does not fill comes back zero (Proem's
# fstat-identity handoff). Driven by scripts/test-crename.py. Runs as /bin/CFSTAT.
cfstat-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/cfstat.c -o $(BUILD_DIR)/pico/cfstat.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cfstat.elf $(PICO_PORT) $(BUILD_DIR)/pico/cfstat.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cfstat.elf $(CFSTAT_BIN)

# The C twin of /bin/ARGS: the argument vector crt0 builds, printed in the Rust
# program's format (step 1 of docs/roadmap/roadmap-c-hosting.md). Runs as
# /bin/CARGS through picolibc, and as /bin/CARGSH (below) through the
# hand-rolled libc, since crt0 is compiled into each with different flags.
cargs-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/cargs.c -o $(BUILD_DIR)/pico/cargs.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cargs.elf $(PICO_PORT) $(BUILD_DIR)/pico/cargs.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cargs.elf $(CARGS_BIN)

cargsh-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cargs.c -o $(BUILD_DIR)/cargsh.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cargsh.elf $(BUILD_DIR)/cargsh.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cargsh.elf $(CARGSH_BIN)

# errno from the file layer: every failure a C program can provoke there,
# checked for POSIX's errno (step 2 of docs/roadmap/roadmap-c-hosting.md).
# Driven by scripts/test-cerrno.py. Runs as /bin/CERRNO.
cerrno-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/cerrno.c -o $(BUILD_DIR)/pico/cerrno.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cerrno.elf $(PICO_PORT) $(BUILD_DIR)/pico/cerrno.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cerrno.elf $(CERRNO_BIN)

# The environment in a C program: environ in /bin/PRINTENV's format, and
# getenv's answers (step 3 of docs/roadmap/roadmap-c-hosting.md). Runs as
# /bin/CENV through picolibc and /bin/CENVH through the hand-rolled libc (no
# getenv there), since crt0 is built into each. Driven by scripts/test-cenv.py.
cenv-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/cenv.c -o $(BUILD_DIR)/pico/cenv.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cenv.elf $(PICO_PORT) $(BUILD_DIR)/pico/cenv.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cenv.elf $(CENV_BIN)

cenvh-bin: $(LIBC_OBJS) $(NSRESOLVE_A)
	$(CC) $(CFLAGS_OS) -Ilibc/include -c libc/cenv.c -o $(BUILD_DIR)/cenvh.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cenvh.elf $(BUILD_DIR)/cenvh.o $(LIBC_OBJS) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cenvh.elf $(CENVH_BIN)

# The clock in a C program (step 5 of the C-hosting plan): time, gettimeofday
# and clock_gettime from libc/pico/clock.c. Runs as /bin/CCLOCK, driven by
# scripts/test-cclock.py.
cclock-bin: $(NSRESOLVE_A) $(PICO_PORT)
	$(CC) $(CFLAGS_OS) $(PICO_INC) -c libc/cclock.c -o $(BUILD_DIR)/pico/cclock.o
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cclock.elf $(PICO_PORT) $(BUILD_DIR)/pico/cclock.o $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cclock.elf $(CCLOCK_BIN)

# The C preprocessor, cpp, from DevTools (step 8 of the C-hosting plan, and the
# build half of the accepted 2026-10-01-from-proem-system-dirs.md): its lib/
# and driver/cpp.c, compiled as every picolibc program here is and linked like
# CPICO, with the two system directories make esp stages built in, so `cpp
# hello.c` on Ouroboros needs no options (it reads /include/target.h first).
# Its sources are DevTools's and are only read here; the link is ours.
# Real objects with their header dependencies (-MMD), so a `make run` rebuilds
# only what changed in cpp's sources, not all of them every time.
# Without sources, `make cpp-bin` stops at parse time (NEED_CPP above), before
# any link is tried.
cpp-bin: $(CPP_BIN)
CPP_CFLAGS := $(CFLAGS_OS) $(PICO_INC) -I$(CPP_DIR)/lib '-DCPP_SYSTEM_DIRS="/include:/include/clang"' -MMD -MP
# The Makefile itself is a prerequisite (CFLAGS_OS, PICO_INC and the system
# directories are spelled here), and the link's are the libc, the port and
# the linker script, so a change to any of them rebuilds /bin/cpp rather than
# shipping it stale (the third review of #226).
$(BUILD_DIR)/cpp/%.o: $(CPP_DIR)/lib/%.c Makefile
	@mkdir -p $(BUILD_DIR)/cpp
	$(CC) $(CPP_CFLAGS) -c $< -o $@
$(BUILD_DIR)/cpp/cpp.o: $(CPP_DIR)/driver/cpp.c Makefile
	@mkdir -p $(BUILD_DIR)/cpp
	$(CC) $(CPP_CFLAGS) -c $< -o $@
$(CPP_BIN): $(CPP_OBJS) $(PICO_PORT) $(NSRESOLVE_A) $(PICO_LIBC) programs/linker.ld
	"$(LD_LLD)" $(LDFLAGS_RUSTSHIM) -o $(BUILD_DIR)/cpp.elf $(PICO_PORT) $(CPP_OBJS) $(PICO_LIBC) $(NSRESOLVE_A)
	"$(OBJCOPY)" --strip-all $(BUILD_DIR)/cpp.elf $(CPP_BIN)
-include $(wildcard $(BUILD_DIR)/cpp/*.d)

write-bin:
	cargo build -p write --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(WRITE_ELF) $(WRITE_BIN)

readkey-bin:
	cargo build -p readkey --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(READKEY_ELF) $(READKEY_BIN)

more-bin:
	cargo build -p more --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(MORE_ELF) $(MORE_BIN)

send-bin:
	cargo build -p send --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(SEND_ELF) $(SEND_BIN)

recv-bin:
	cargo build -p recv --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RECV_ELF) $(RECV_BIN)

selftest-bin:
	cargo build -p selftest --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(SELFTEST_ELF) $(SELFTEST_BIN)

bootid-bin:
	cargo build -p bootid --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(BOOTID_ELF) $(BOOTID_BIN)

# The ed25519 on-target check (docs/roadmap/roadmap-cluster-keys.md step 5): the same RFC
# 8032 vectors the host tests run, plus peak-stack and per-operation timing,
# which only the target can answer.
edtest-bin:
	cargo build -p edtest --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(EDTEST_ELF) $(EDTEST_BIN)

keyprobe-bin:
	cargo build -p keyprobe --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(KEYPROBE_ELF) $(KEYPROBE_BIN)

man-bin:
	cargo build -p man --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(MAN_ELF) $(MAN_BIN)

ping-bin:
	cargo build -p ping --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PING_ELF) $(PING_BIN)

wc-bin:
	cargo build -p wc --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(WC_ELF) $(WC_BIN)

grep-bin:
	cargo build -p grep --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(GREP_ELF) $(GREP_BIN)

head-bin:
	cargo build -p head --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(HEAD_ELF) $(HEAD_BIN)

tail-bin:
	cargo build -p tail --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(TAIL_ELF) $(TAIL_BIN)

nl-bin:
	cargo build -p nl --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(NL_ELF) $(NL_BIN)

rev-bin:
	cargo build -p rev --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(REV_ELF) $(REV_BIN)

uniq-bin:
	cargo build -p uniq --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(UNIQ_ELF) $(UNIQ_BIN)

sort-bin:
	cargo build -p sort --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(SORT_ELF) $(SORT_BIN)

resolve-bin:
	cargo build -p resolve --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RESOLVE_ELF) $(RESOLVE_BIN)

fetch-bin:
	cargo build -p fetch --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(FETCH_ELF) $(FETCH_BIN)

dial-bin:
	cargo build -p dial --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(DIAL_ELF) $(DIAL_BIN)

serve-bin:
	cargo build -p serve --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(SERVE_ELF) $(SERVE_BIN)

# Stage the EFI System Partition layout QEMU/Parallels expect: a removable
# UEFI drive boots \EFI\BOOT\BOOTAA64.EFI automatically, no boot manager
# entry needed. \EFI\ORBS\ (must fit FAT's 8.3 short-name limit, which
# the full 9-character project name doesn't - see loader.rs's
# CONFIG_PATH doc comment for why) holds everything that isn't the kernel
# itself: the default shell binary and the config file (loader.rs's
# CONFIG_PATH) naming which program to load - edit INIT.CFG and rebuild
# just that program to swap it out, no kernel rebuild required.
# STAGED FROM SCRATCH, NOT ACCRETED, and that is the whole point of the `rm -rf`.
#
# This recipe only ever ADDS files, so deleting a staging line does not delete
# what it already wrote - only `make clean` did. The shared cluster key was
# staged here until the flag day, so any tree built before it still held
# \CLUSTER.KEY and `make image` kept baking the dev secret into esp.img,
# esp.hdd and both two-node images, while the flag day's headline check is
# "an image with no CLUSTER.KEY anywhere on it".
#
# The first fix was a two-entry blocklist (CLUSTER.KEY and a stale `User/` from
# the /User -> /Users rename). It was ALREADY INCOMPLETE when it was written:
# build/esp/bin/PONG was staged by no line in this file - a survivor of an older
# path - and because $(EXT2_PART)/$(EXFAT_PART) copy $(ESP_DIR)/bin/* wholesale,
# it had propagated into the payload images too. Naming the survivors you happen
# to know is not a fix for "the directory remembers everything"; regenerating it
# is. Every path below is written by this recipe, and the run targets that mount
# $(ESP_DIR) with QEMU's `fat:rw:` can add files of their own, which is a second
# way in that a blocklist would never have covered.
#
# THE DELETE IS GUARDED, because `ESP_DIR` is not a literal. It derives from
# `BUILD_DIR`, both are ordinary simply-expanded variables, and a command-line
# assignment overrides them - the idiom this Makefile documents for
# `CLUSTER_NODE`. An earlier version of this comment claimed the opposite ("a
# literal, never user-supplied") while `make esp ESP_DIR=$HOME` would have
# expanded the line below to `rm -rf $HOME`. A claim in a comment is not a
# check, so it is a check now.
#
# SCOPE, stated so it is not mistaken for more: the guard and the `rm -rf` are
# quoted, which is what makes the destructive line safe. The `mkdir`/`cp` lines
# below are not, so a BUILD_DIR containing whitespace fails the build noisily
# (and can leave a stray directory) rather than deleting anything. That is the
# right trade at 70-odd paths; quoting them all is churn without a hazard.
esp: build shell-bin hello-bin pong-bin fsd-bin upper-bin cond-bin netd-bin accountd-bin args-bin echo-bin uptime-bin clear-bin ls-bin cat-bin mkdir-bin rmdir-bin touch-bin rm-bin cp-bin mv-bin writeat-bin chmod-bin chown-bin tree-bin pwd-bin printenv-bin rdprobe-bin fpprobe-bin vtprobe-bin id-bin passwd-bin useradd-bin groupadd-bin usermod-bin clusterkey-bin chello-bin cdemo-bin cfile-bin cleak-bin cfidhold-bin nsdemo-bin cremote-bin cbig-bin cwrite-bin cpico-bin cmem-bin crename-bin cfstat-bin cargs-bin cargsh-bin cerrno-bin cenv-bin cenvh-bin cclock-bin write-bin readkey-bin more-bin send-bin recv-bin selftest-bin bootid-bin edtest-bin keyprobe-bin man-bin ping-bin resolve-bin fetch-bin dial-bin serve-bin wc-bin grep-bin head-bin tail-bin nl-bin rev-bin uniq-bin sort-bin $(if $(HAVE_CPP),cpp-bin)
	@test ! -e "$(ESP_DIR)" || test -f "$(ESP_DIR)/EFI/ORBS/INIT.CFG" || { \
		echo "esp: $(ESP_DIR) is not an Ouroboros ESP tree - refusing to delete it"; \
		echo "esp: (remove it by hand if that is really where you want the ESP staged)"; \
		exit 1; }
	rm -rf "$(ESP_DIR)"
	mkdir -p $(ESP_DIR)/EFI/BOOT $(ESP_DIR)/EFI/ORBS $(ESP_DIR)/bin $(ESP_DIR)/man $(ESP_DIR)/etc
	# The C headers, in two system directories (step 6 of the C-hosting plan,
	# and the accepted 2026-10-01-from-proem-system-dirs.md): /include holds
	# picolibc's tree and target.h, the predefined macros of exactly the flags
	# a picolibc program here is compiled with (there is no clang here to ask);
	# /include/clang holds clang's freestanding headers. Two because
	# inttypes.h, limits.h, stdint.h and stdnoreturn.h exist on both sides and
	# picolibc's limits.h reaches clang's by #include_next. The ext2 and exFAT
	# images copy this tree too.
	mkdir -p $(ESP_DIR)/include/clang
	cp -R $(PICO_DIR)/include/. $(ESP_DIR)/include/
	$(CC) $(CFLAGS_OS) $(PICO_INC) -dM -E -x c /dev/null > $(ESP_DIR)/include/target.h
	res=$$($(CC) -print-resource-dir) && [ -n "$$res" ] && [ -d "$$res/include" ] || { \
		echo "esp: no clang resource directory from '$(CC) -print-resource-dir'"; exit 1; }; \
	cd "$$res/include" && cp $(CLANG_HDRS) "$(abspath $(ESP_DIR))/include/clang/" && \
	for f in $(CLANG_GLOBS); do \
		[ ! -e "$$f" ] || cp "$$f" "$(abspath $(ESP_DIR))/include/clang/" || exit 1; \
	done
	cp $(KERNEL) $(ESP_DIR)/EFI/BOOT/BOOTAA64.EFI
	cp $(SHELL_BIN) $(ESP_DIR)/EFI/ORBS/SH.BIN
	cp $(HELLO_BIN) $(ESP_DIR)/EFI/ORBS/HELLO.BIN
	cp $(PONG_BIN) $(ESP_DIR)/EFI/ORBS/PONG.BIN
	cp $(FSD_BIN) $(ESP_DIR)/EFI/ORBS/FSD.BIN
	cp $(UPPER_BIN) $(ESP_DIR)/EFI/ORBS/UPPER.BIN
	cp $(UPPER_BIN) $(ESP_DIR)/bin/UPPER
	cp $(COND_BIN) $(ESP_DIR)/EFI/ORBS/COND.BIN
	cp $(NETD_BIN) $(ESP_DIR)/EFI/ORBS/NETD.BIN
	cp $(ACCTD_BIN) $(ESP_DIR)/EFI/ORBS/ACCOUNTD.BIN
	cp $(ARGS_BIN) $(ESP_DIR)/EFI/ORBS/ARGS.BIN
	printf '\\EFI\\ORBS\\SH.BIN' > $(ESP_DIR)/EFI/ORBS/INIT.CFG
	# The boot counter's ESP store (kernel/src/bootid.rs): one fixed-width
	# record, staged at 0 so the first boot of a new image finds a counter
	# from "before this boot" and raises it to 1. Restaging it is a ROLLBACK of
	# the counter, accepted for a dev image and stated in Decision 6 of
	# docs/roadmap/roadmap-session-auth.md.
	printf '%020d\n' 0 > $(ESP_DIR)/EFI/ORBS/BOOTID.TXT
	# /bin: programs the shell finds via PATH by bare name (Stage 2 of the
	# standalone-binaries arc). Named uppercase, no extension (8.3-legal);
	# fsd's case-insensitive lookup matches a lowercase-typed command. For
	# now the argv proof program plus the first externalized commands
	# (echo/uptime/clear - Stage 4), each a real program sharing `ulib`.
	cp $(ARGS_BIN) $(ESP_DIR)/bin/ARGS
	cp $(ECHO_BIN) $(ESP_DIR)/bin/ECHO
	cp $(UPTIME_BIN) $(ESP_DIR)/bin/UPTIME
	cp $(CLEAR_BIN) $(ESP_DIR)/bin/CLEAR
	cp $(LS_BIN) $(ESP_DIR)/bin/LS
	cp $(CAT_BIN) $(ESP_DIR)/bin/CAT
	cp $(MKDIR_BIN) $(ESP_DIR)/bin/MKDIR
	cp $(RMDIR_BIN) $(ESP_DIR)/bin/RMDIR
	cp $(TOUCH_BIN) $(ESP_DIR)/bin/TOUCH
	cp $(RM_BIN) $(ESP_DIR)/bin/RM
	cp $(CP_BIN) $(ESP_DIR)/bin/CP
	cp $(MV_BIN) $(ESP_DIR)/bin/MV
	cp $(WRITEAT_BIN) $(ESP_DIR)/bin/WRITEAT
	cp $(CHMOD_BIN) $(ESP_DIR)/bin/CHMOD
	cp $(CHOWN_BIN) $(ESP_DIR)/bin/CHOWN
	cp $(TREE_BIN) $(ESP_DIR)/bin/TREE
	cp $(PWD_BIN) $(ESP_DIR)/bin/PWD
	cp $(PRINTENV_BIN) $(ESP_DIR)/bin/PRINTENV
	cp $(RDPROBE_BIN) $(ESP_DIR)/bin/RDPROBE
	cp $(FPPROBE_BIN) $(ESP_DIR)/bin/FPPROBE
	cp $(VTPROBE_BIN) $(ESP_DIR)/bin/VTPROBE
	cp $(ID_BIN) $(ESP_DIR)/bin/ID
	cp $(PASSWD_BIN) $(ESP_DIR)/bin/PASSWD
	cp $(USERADD_BIN) $(ESP_DIR)/bin/USERADD
	cp $(GROUPADD_BIN) $(ESP_DIR)/bin/GROUPADD
	cp $(USERMOD_BIN) $(ESP_DIR)/bin/USERMOD
	cp $(CLUSTERKEY_BIN) $(ESP_DIR)/bin/CLUSTERKEY
	cp $(CHELLO_BIN) $(ESP_DIR)/bin/CHELLO
	cp $(CDEMO_BIN) $(ESP_DIR)/bin/CDEMO
	cp $(CFILE_BIN) $(ESP_DIR)/bin/CFILE
	cp $(CLEAK_BIN) $(ESP_DIR)/bin/CLEAK
	cp $(CFIDHOLD_BIN) $(ESP_DIR)/bin/CFIDHOLD
	cp $(NSDEMO_BIN) $(ESP_DIR)/bin/NSDEMO
	cp $(CREMOTE_BIN) $(ESP_DIR)/bin/CREMOTE
	cp $(CBIG_BIN) $(ESP_DIR)/bin/CBIG
	cp $(CWRITE_BIN) $(ESP_DIR)/bin/CWRITE
	cp $(CPICO_BIN) $(ESP_DIR)/bin/CPICO
	cp $(CMEM_BIN) $(ESP_DIR)/bin/CMEM
	cp $(CRENAME_BIN) $(ESP_DIR)/bin/CRENAME
	cp $(CFSTAT_BIN) $(ESP_DIR)/bin/CFSTAT
	cp $(CARGS_BIN) $(ESP_DIR)/bin/CARGS
	cp $(CARGSH_BIN) $(ESP_DIR)/bin/CARGSH
	cp $(CERRNO_BIN) $(ESP_DIR)/bin/CERRNO
	cp $(CENV_BIN) $(ESP_DIR)/bin/CENV
	cp $(CENVH_BIN) $(ESP_DIR)/bin/CENVH
	cp $(CCLOCK_BIN) $(ESP_DIR)/bin/CCLOCK
	# /bin/cpp, when CPP_DIR has DevTools's sources; a checkout of this tree
	# alone still builds an image, without it.
	$(if $(HAVE_CPP),cp $(CPP_BIN) $(ESP_DIR)/bin/CPP,@echo "esp: no cpp sources at $(CPP_DIR); /bin/cpp not staged (set CPP_DIR)")
	cp $(WRITE_BIN) $(ESP_DIR)/bin/WRITE
	cp $(READKEY_BIN) $(ESP_DIR)/bin/READKEY
	cp $(MORE_BIN) $(ESP_DIR)/bin/MORE
	cp $(MORE_BIN) $(ESP_DIR)/bin/LESS
	cp $(SEND_BIN) $(ESP_DIR)/bin/SEND
	cp $(RECV_BIN) $(ESP_DIR)/bin/RECV
	cp $(SELFTEST_BIN) $(ESP_DIR)/bin/SELFTEST
	cp $(BOOTID_BIN) $(ESP_DIR)/bin/BOOTID
	cp $(EDTEST_BIN) $(ESP_DIR)/bin/EDTEST
	cp $(KEYPROBE_BIN) $(ESP_DIR)/bin/KEYPROBE
	cp $(MAN_BIN) $(ESP_DIR)/bin/MAN
	cp $(PING_BIN) $(ESP_DIR)/bin/PING
	cp $(RESOLVE_BIN) $(ESP_DIR)/bin/RESOLVE
	cp $(FETCH_BIN) $(ESP_DIR)/bin/FETCH
	cp $(DIAL_BIN) $(ESP_DIR)/bin/DIAL
	cp $(SERVE_BIN) $(ESP_DIR)/bin/SERVE
	cp $(WC_BIN) $(ESP_DIR)/bin/WC
	cp $(GREP_BIN) $(ESP_DIR)/bin/GREP
	cp $(HEAD_BIN) $(ESP_DIR)/bin/HEAD
	cp $(TAIL_BIN) $(ESP_DIR)/bin/TAIL
	cp $(NL_BIN) $(ESP_DIR)/bin/NL
	cp $(REV_BIN) $(ESP_DIR)/bin/REV
	cp $(UNIQ_BIN) $(ESP_DIR)/bin/UNIQ
	cp $(SORT_BIN) $(ESP_DIR)/bin/SORT
	# Manual pages: plain-text files read by /bin/MAN as /man/<command>.
	cp manpages/* $(ESP_DIR)/man/
	# /etc/cluster: this machine's Ed25519 identity and the peers it accepts
	# (docs/roadmap/roadmap-cluster-keys.md). netd READS `authorized` at boot as of
	# step 7 and verifies signed frames against it; `id` becomes load-bearing at
	# step 8, when this machine starts signing its own outbound requests.
	mkdir -p $(ESP_DIR)/etc/cluster
	python3 scripts/mkclusterkeys.py $(ESP_DIR)/etc/cluster $(CLUSTER_NODE)
	# /etc/passwd + /etc/group: the account database the shell's login gate and
	# the /bin account tools (id/su/passwd/useradd/groupadd/usermod) use
	# (name:uid:gid:home:salt:hash / name:gid:members, hashes precomputed - see
	# scripts/mkpasswd.py + scripts/mkgroup.py; DEV creds root/root, user/user, guest/guest).
	# Absent -> login falls back to root.
	python3 scripts/mkpasswd.py > $(ESP_DIR)/etc/passwd
	python3 scripts/mkpasswd.py --shadow > $(ESP_DIR)/etc/shadow
	chmod 600 $(ESP_DIR)/etc/shadow
	python3 scripts/mkgroup.py > $(ESP_DIR)/etc/group
	# Per-user home directories under /Users (the login home for `user`; `~`
	# expands to it). FAT can't record an owner, so it's world-usable here; on
	# ext2 the image build chowns it (see the ext2-src staging).
	mkdir -p $(ESP_DIR)/Users/user $(ESP_DIR)/Users/guest

# Boots the ESP directory directly in QEMU (no disk image needed) against
# the aarch64 OVMF firmware installed by `brew install qemu`.
#
# The drive is explicitly attached as a virtio-mmio block device
# (if=none + -device virtio-blk-device), not left to QEMU's own default -
# a plain `-drive ...,media=disk` on this machine type auto-attaches as
# virtio-blk-*pci* instead, which firmware boots from just as well but
# would need this kernel's own runtime driver to walk PCI/ECAM config
# space to find (a real subsystem on its own - see virtio_mmio.rs's
# module doc comment). virtio-mmio.force-legacy=false selects the modern
# (non-legacy) register interface - QEMU defaults virtio-mmio to legacy
# mode, confirmed via `-device virtio-mmio,help`'s printed default, kept
# only for old-guest compatibility this project has no need to imitate.
# `-machine` for `run`: `run-el2` overrides it (a target-specific variable
# reaches the prerequisite's recipe), so the two boot one device list.
MACHINE ?= virt
run: esp
	qemu-system-aarch64 \
		-machine $(MACHINE) \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-nographic

# `run` with the firmware handing the kernel off at EL2, as the Raspberry
# Pi's does (virtualization=on gives the guest an EL2 and the firmware runs
# there): the dev loop for the EL1 drop, docs/roadmap/roadmap-el1-drop.md.
# The kernel's `running at EL2 after the exit` line, then `dropped from EL2
# to EL1`, is the check. Not a copy of `run`: the same recipe with MACHINE
# overridden, so a change to `run`'s devices reaches this boot too.
run-el2: override MACHINE := $(MACHINE),virtualization=on
run-el2: run

# Same as `run`, plus a virtio-net device on virtio-mmio with QEMU's
# user-mode (SLIRP) networking - the dev loop for the network stack
# (kernel/src/virtio_net.rs, docs/ROADMAP.md's Stage 1). SLIRP answers ARP
# for its gateway (10.0.2.2), which is what init_net's boot-time probe
# exercises. `-object filter-dump` writes every frame to $(NET_PCAP) for
# independent host-side inspection (tcpdump/tshark), the same "verify against
# a source outside the kernel's own output" discipline used elsewhere.
run-net: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev user,id=net0 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Same as `run`, plus a virtio-mmio console device (device ID 3, the
# virtio-console/virtio-serial class - see kernel/src/virtio_console.rs)
# attached via a separate chardev, for testing that driver. On this
# machine (default OVMF firmware, ACPI present), devicetree/ACPI/PCI
# console discovery always wins before virtio_console.rs ever gets a
# chance to run - this target only makes the *device* available, it
# doesn't organically force the fallback to trigger. To actually
# exercise it, temporarily force main.rs's `if !found_console_early`
# check to `if true` (see CLAUDE.md's virtio-console section for how
# this was originally verified, and why acpi=off doesn't work for this -
# it makes devicetree discovery succeed instead, a real surprise, not a
# path to "everything fails").
run-virtio-console: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-device virtio-serial-device \
		-device virtconsole,chardev=vcon0 \
		-chardev file,id=vcon0,path=$(BUILD_DIR)/vcon.log \
		-nographic

# Same as `run`, plus a real xHCI (USB3) host controller with a virtual
# USB HID keyboard attached (kernel/src/xhci.rs) and a QEMU HMP monitor
# socket for injecting keystrokes with `sendkey` (there's no way to type
# into an emulated USB keyboard via piped stdin the way the PL011 console
# tests inject bytes - this is genuinely different hardware). Unlike
# `run-virtio-console`, no source-level force is needed to exercise this:
# the xHCI driver isn't gated behind console discovery at all (see
# xhci.rs's module doc comment), so it always runs on this target.
run-usb-kbd: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-device qemu-xhci,id=xhci0 \
		-device usb-kbd,bus=xhci0.0 \
		-monitor unix:$(BUILD_DIR)/qemu-monitor.sock,server,nowait \
		-nographic

# Same as `run-usb-kbd`, plus two more USB devices on the same xHCI
# controller: a usb-tablet (a stand-in for Parallels' virtual mouse - a
# HID device that is *not* a keyboard) and a usb-storage stick backed by
# a small scratch image (created on demand) - the three-device rig for
# exercising xhci.rs's multi-device port scan. The storage device should
# show up in the boot log as a recognized-but-not-driven mass storage
# interface (class 0x08); the keyboard must still type normally via the
# monitor's `sendkey`.
# A real FAT32 image for the USB stick (not the old zeroed scratch file)
# so the mass-storage driver has something to actually mount - built the
# same hdiutil way as build/esp.img, with a marker file to ls/cat. 64MB: small
# hdiutil FAT32 requests can silently produce FAT16 (same class of
# surprise as make run's vvfat - see fat32.rs's module doc comment).
$(USBSTICK_IMG):
	mkdir -p $(BUILD_DIR)
	rm -rf $(BUILD_DIR)/usbstick-src && mkdir $(BUILD_DIR)/usbstick-src
	printf 'hello from the USB stick\n' > $(BUILD_DIR)/usbstick-src/USBTEST.TXT
	hdiutil create -size 64m -fs FAT32 -volname USBSTICK -srcfolder $(BUILD_DIR)/usbstick-src -format UDTO -ov $(BUILD_DIR)/usbstick.cdr
	mv $(BUILD_DIR)/usbstick.cdr $(USBSTICK_IMG)
	rm -rf $(BUILD_DIR)/usbstick-src

run-usb-multi: esp $(USBSTICK_IMG)
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-device qemu-xhci,id=xhci0 \
		-device usb-kbd,bus=xhci0.0 \
		-device usb-tablet,bus=xhci0.0 \
		-drive file=$(USBSTICK_IMG),format=raw,if=none,id=usbstick \
		-device usb-storage,drive=usbstick,bus=xhci0.0 \
		-monitor unix:$(BUILD_DIR)/qemu-monitor.sock,server,nowait \
		-nographic

# The Raspberry Pi's USB layout, as far as QEMU can model it: a usb-hub on
# root port 1 with the keyboard and the storage stick BEHIND it (every USB 2.0
# device on a Pi 4/400 sits behind an on-board hub - docs/testing/testing-pi4.md
# section 1b), and a usb-tablet directly on root port 2. QEMU's hub is
# full-speed, so the Pi's high-speed hub with a slower keyboard below it (the
# transaction-translator case) is not modelled. `make test-usb-hub` is the
# scripted check of the same layout.
run-usb-hub: esp $(USBSTICK_IMG)
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-device qemu-xhci,id=xhci0 \
		-device usb-hub,bus=xhci0.0,port=1 \
		-device usb-kbd,bus=xhci0.0,port=1.1 \
		-drive file=$(USBSTICK_IMG),format=raw,if=none,id=usbstick \
		-device usb-storage,drive=usbstick,bus=xhci0.0,port=1.2 \
		-device usb-tablet,bus=xhci0.0,port=2 \
		-monitor unix:$(BUILD_DIR)/qemu-monitor.sock,server,nowait \
		-nographic

# Same as `run`, but forces QEMU's virt machine onto GICv3 instead of its
# default GICv2 (`-machine virt,help` confirms `gic-version` accepts
# 2/3/4/x-5/host/max - real, checked, not assumed). For exercising
# kernel/src/gicv3.rs and kernel/src/madt.rs's GICv3 discovery path without
# needing real Parallels hardware for every iteration - see CLAUDE.md's
# MADT/GICv3 scoping notes. Confirmed via a devicetree dump
# (`qemu-system-aarch64 -machine virt,gic-version=3,dumpdtb=...`) that this
# QEMU build describes GICv3 as one contiguous GICR region (GICD @
# 0x08000000, GICR @ 0x080a0000 size 0xf60000), not per-CPU GICC.GICR
# fields - madt.rs's parser confirmed independently to report the exact
# same addresses from the real ACPI MADT, not just the devicetree.
run-gicv3: esp
	qemu-system-aarch64 \
		-machine virt,gic-version=3 \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Builds a real MBR+FAT32 .img (a valid raw UEFI-bootable disk - verified by
# booting it directly in QEMU with -drive format=raw, not just the vvfat
# passthrough `run` uses). This is NOT directly attachable in Parallels: its
# Hard Disk device only accepts Parallels' own .hdd format. For Parallels,
# use `make parallels-hdd` instead, which wraps this image into one.
image: esp
	rm -f $(ESP_DIR).img
	hdiutil create -size 64m -fs FAT32 -volname OUROBOROS -srcfolder $(ESP_DIR) -format UDTO -ov $(ESP_DIR).cdr
	mv $(ESP_DIR).cdr $(ESP_DIR).img
	# Strip the macOS AppleDouble sidecars (._* files) hdiutil sprinkles
	# onto the FAT volume during the copy - FAT can't hold extended
	# attributes, so macOS spills them into these files, which then show up
	# in the shell's `ls` as mangled 8.3 aliases (_EFI~1, etc.) since the
	# FAT32 reader doesn't decode long filenames. Attach the flat image
	# read-write, delete them, detach.
	MP=$$(mktemp -d); \
	hdiutil attach -nobrowse -mountpoint "$$MP" $(ESP_DIR).img >/dev/null; \
	find "$$MP" -name '._*' -delete; \
	hdiutil detach "$$MP" >/dev/null; \
	rmdir "$$MP"

# build/esp.img again with the MSDSTALL boot flag file at its root, for
# `test-usb-hub.py --stall` (see bootflags.rs): the same attach, change,
# strip-sidecars, detach as `image`. The detach is always attempted, forced
# if it is refused, and a failure at any step fails the target.
image-stall: image
	cp $(ESP_DIR).img $(STALL_IMG)
	@MP=$$(mktemp -d); \
	hdiutil attach -nobrowse -mountpoint "$$MP" $(STALL_IMG) >/dev/null || { rmdir "$$MP"; exit 1; }; \
	ok=0; touch "$$MP/MSDSTALL" && find "$$MP" -name '._*' -delete && ok=1; \
	hdiutil detach "$$MP" >/dev/null || hdiutil detach -force "$$MP" >/dev/null || { echo "image-stall: $$MP is still mounted"; exit 1; }; \
	rmdir "$$MP"; \
	test $$ok = 1 || { echo "image-stall: could not stage MSDSTALL"; exit 1; }

# Boots the real build/esp.img (genuine FAT32) instead of `run`'s vvfat
# passthrough - needed for anything that reads the filesystem at runtime
# (fat32.rs and up), not just the fast kernel-dev loop `run` is for.
# **`run`'s vvfat is FAT16, not FAT32** - confirmed by decoding its BPB
# directly (BS_FilSysType literally reads "FAT16   ", and RootEntryCount/
# FATSz16 are both nonzero, which real FAT32 requires to be zero): QEMU's
# vvfat driver apparently can't produce FAT32 at all. build/esp.img (built by
# `hdiutil -fs FAT32`, what Parallels ultimately boots from too via
# `parallels-hdd`) is genuinely FAT32 - confirmed the same way, decoding
# its BPB directly with `xxd` before writing any parser code. Use this
# target, not `run`, whenever the on-disk filesystem format actually
# matters.
run-image: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-nographic

# $(GPT_IMG): build/esp.img's FAT32 partition wrapped in a *GPT* disk (protective
# MBR + primary/backup GPT headers, an EFI-System-Partition entry) - built by
# scripts/mkgpt.py because macOS has no GPT tooling and hdiutil only makes MBR.
# For testing fsd's GPT partition discovery (the more-filesystems arc).
image-gpt: image
	python3 scripts/mkgpt.py

# Boot the GPT disk instead of the MBR build/esp.img: UEFI boots BOOTAA64 from the
# ESP, and fsd discovers the FAT32 partition through the GPT path (the disk has
# no real MBR partition table, only a protective 0xEE entry).
run-image-gpt: image-gpt
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(GPT_IMG),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-nographic

# $(EXFAT_PART): a raw exFAT filesystem holding /bin (the externalized commands,
# so the shell can run them off exFAT) plus a few test files, built with macOS's
# newfs_exfat (hdiutil can't make exFAT). This is the payload scripts/mkexfat.py
# drops into the exFAT partition of the combined disk. For testing the exFAT
# reader (fsd/src/exfat.rs, the more-filesystems arc's read-only exFAT step).
$(EXFAT_PART): esp
	rm -rf $(BUILD_DIR)/exfat-src && mkdir -p $(BUILD_DIR)/exfat-src/bin
	cp $(ESP_DIR)/bin/* $(BUILD_DIR)/exfat-src/bin/
	cp -R $(ESP_DIR)/include $(BUILD_DIR)/exfat-src/include
	# Manual pages, read by /bin/MAN as /man/<command> (exFAT matches case-
	# insensitively, like FAT, so the lowercase source names stage as-is).
	mkdir -p $(BUILD_DIR)/exfat-src/man
	cp manpages/* $(BUILD_DIR)/exfat-src/man/
	mkdir -p $(BUILD_DIR)/exfat-src/etc
	python3 scripts/mkpasswd.py > $(BUILD_DIR)/exfat-src/etc/passwd
	# /etc/shadow must be staged wherever /etc/passwd is: passwd no longer
	# carries the secret, so an image with one and not the other boots to a
	# login prompt NO password can satisfy (the file is non-empty, so login's
	# root-session fallback doesn't fire either). exFAT records no mode - see
	# the login-time warning.
	python3 scripts/mkpasswd.py --shadow > $(BUILD_DIR)/exfat-src/etc/shadow
	python3 scripts/mkgroup.py > $(BUILD_DIR)/exfat-src/etc/group
	mkdir -p $(BUILD_DIR)/exfat-src/Users/user $(BUILD_DIR)/exfat-src/Users/guest
	printf 'hello from an exFAT volume\r\n' > $(BUILD_DIR)/exfat-src/HELLO.TXT
	printf 'line one\r\nline two has several words\r\nthird and final line\r\n' > $(BUILD_DIR)/exfat-src/README.TXT
	mkdir -p $(BUILD_DIR)/exfat-src/SUB
	printf 'a nested file on exFAT\r\n' > $(BUILD_DIR)/exfat-src/SUB/NESTED.TXT
	printf 'this exercises a long UTF-16 name\r\n' > "$(BUILD_DIR)/exfat-src/a-long-exfat-name.txt"
	rm -f $(EXFAT_PART)
	dd if=/dev/zero of=$(EXFAT_PART) bs=1m count=24 2>/dev/null
	DEV=$$(hdiutil attach -nomount -imagekey diskimage-class=CRawDiskImage $(EXFAT_PART) | head -1 | awk '{print $$1}'); \
	newfs_exfat -v OUROEXFAT "$$DEV" >/dev/null; \
	diskutil mount "$$DEV" >/dev/null; \
	MP=$$(diskutil info "$$DEV" | awk -F': *' '/Mount Point/{print $$2}'); \
	cp -R $(BUILD_DIR)/exfat-src/. "$$MP/"; \
	find "$$MP" -name '._*' -delete; rm -rf "$$MP/.fseventsd" "$$MP/.Trashes" "$$MP/.Spotlight-V100"; \
	diskutil unmount "$$DEV" >/dev/null; hdiutil detach "$$DEV" >/dev/null
	rm -rf $(BUILD_DIR)/exfat-src

# $(EXFAT_IMG): a two-partition MBR disk - partition 1 exFAT (fsd mounts it),
# partition 2 the FAT32 ESP (UEFI boots it). See scripts/mkexfat.py for why the
# exFAT partition must come first. Exercises fsd's Filesystem-enum fallthrough
# (FAT32 probe fails on the exFAT partition, exFAT probe succeeds).
image-exfat: image $(EXFAT_PART)
	python3 scripts/mkexfat.py

# Boot the combined disk: UEFI boots BOOTAA64 from the FAT32 ESP (partition 2),
# then fsd mounts the exFAT partition (partition 1) - so `ls`/`cat`/pipelines
# all read from exFAT (their /bin binaries live there too).
run-image-exfat: image-exfat
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(EXFAT_IMG),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-nographic

# $(EXT2_PART): a raw ext2 filesystem holding /bin (so the shell runs off ext2)
# plus test files, built with Homebrew e2fsprogs' mke2fs (macOS has no native
# ext2 tooling). Block size forced to 1024 so a >12 KiB file (the /bin/CAT
# binary) spills past the 12 direct block pointers into single-indirect blocks -
# exercising fsd/src/ext2.rs's indirection. The payload scripts/mkext2.py drops
# into the ext2 partition of the combined disk.
$(EXT2_PART): esp
	@test -x "$(MKE2FS)" || { echo "mke2fs not found - run: brew install e2fsprogs"; exit 1; }
	mkdir -p $(BUILD_DIR)
	rm -rf $(BUILD_DIR)/ext2-src && mkdir -p $(BUILD_DIR)/ext2-src/bin
	# ext2 is case-sensitive (Unix), and the shell probes /bin/<command> as
	# typed (lowercase), so /bin gets lowercase names here - unlike the FAT/
	# exFAT images, whose 8.3-heritage uppercase names only work because those
	# filesystems match case-insensitively.
	for f in $(ESP_DIR)/bin/*; do cp "$$f" "$(BUILD_DIR)/ext2-src/bin/$$(basename "$$f" | tr A-Z a-z)"; done
	# The C headers as the ESP stages them: already lowercase, and ext2 is
	# case-sensitive, so they copy as they are.
	cp -R $(ESP_DIR)/include $(BUILD_DIR)/ext2-src/include
	# Manual pages, read by /bin/MAN as /man/<command>. The source filenames are
	# already lowercase and `man <cmd>` reads the name verbatim, so - unlike /bin
	# (uppercase, lowercased above) - these stage as-is and resolve case-sensitively.
	mkdir -p $(BUILD_DIR)/ext2-src/man
	cp manpages/* $(BUILD_DIR)/ext2-src/man/
	mkdir -p $(BUILD_DIR)/ext2-src/etc
	python3 scripts/mkpasswd.py > $(BUILD_DIR)/ext2-src/etc/passwd
	# mke2fs -d copies the host mode across, which is how /etc/shadow ends up
	# 0600 on the guest - and, with fsd's enforcement live, actually unreadable
	# to a non-root user rather than merely marked so.
	python3 scripts/mkpasswd.py --shadow > $(BUILD_DIR)/ext2-src/etc/shadow
	chmod 600 $(BUILD_DIR)/ext2-src/etc/shadow
	python3 scripts/mkgroup.py > $(BUILD_DIR)/ext2-src/etc/group
	# /etc/cluster: the per-machine identity. ext2 is the one image where fsd
	# ENFORCES modes, so it is also the only one where `id` being 0600 means
	# anything - and mke2fs -d carries the host's mode onto the guest.
	mkdir -p $(BUILD_DIR)/ext2-src/etc/cluster
	python3 scripts/mkclusterkeys.py $(BUILD_DIR)/ext2-src/etc/cluster $(CLUSTER_NODE)
	mkdir -p $(BUILD_DIR)/ext2-src/Users/user $(BUILD_DIR)/ext2-src/Users/guest
	printf 'only user may read this\n' > $(BUILD_DIR)/ext2-src/Users/user/PRIVATE.TXT
	chmod 600 $(BUILD_DIR)/ext2-src/Users/user/PRIVATE.TXT
	printf 'hello from an ext2 volume\n' > $(BUILD_DIR)/ext2-src/HELLO.TXT
	printf 'line one\nline two has several words\nthird and final line\n' > $(BUILD_DIR)/ext2-src/README.TXT
	mkdir -p $(BUILD_DIR)/ext2-src/sub
	printf 'a nested file on ext2\n' > $(BUILD_DIR)/ext2-src/sub/NESTED.TXT
	printf 'ext2 is case-sensitive, unlike FAT\n' > $(BUILD_DIR)/ext2-src/CaseSensitive.txt
	rm -f $(EXT2_PART)
	"$(MKE2FS)" -q -t ext2 -b 1024 -d $(BUILD_DIR)/ext2-src -F $(EXT2_PART) 24m
	# mke2fs -d stages files root-owned; chown the home dir to `user` (uid/gid
	# 1000) so a logged-in user can write in ~ - the permission-enforcement demo
	# (login user; touch ~/f works, touch / is denied). ext2-only (FAT can't
	# record an owner). debugfs ships with e2fsprogs alongside mke2fs.
	"$(DEBUGFS)" -w -R "sif /Users/user uid 1000" $(EXT2_PART) 2>/dev/null
	"$(DEBUGFS)" -w -R "sif /Users/user gid 1000" $(EXT2_PART) 2>/dev/null
	"$(DEBUGFS)" -w -R "sif /Users/guest uid 1001" $(EXT2_PART) 2>/dev/null
	"$(DEBUGFS)" -w -R "sif /Users/guest gid 1001" $(EXT2_PART) 2>/dev/null
	# /Users/user/PRIVATE.TXT: a file only `user` may read (0600, uid/gid 1000).
	# The impersonation gate's row 2 reads it while CLAIMING to be `user` from a
	# peer `user` never logged in on (docs/roadmap/roadmap-user-keys.md, step 0).
	# The mode is carried by mke2fs -d; the owner has to be set here.
	"$(DEBUGFS)" -w -R "sif /Users/user/PRIVATE.TXT uid 1000" $(EXT2_PART) 2>/dev/null
	"$(DEBUGFS)" -w -R "sif /Users/user/PRIVATE.TXT gid 1000" $(EXT2_PART) 2>/dev/null
	rm -rf $(BUILD_DIR)/ext2-src

# build/espext2.img: a two-partition MBR disk - partition 1 ext2 (fsd mounts it),
# partition 2 the FAT32 ESP (UEFI boots it). See scripts/mkext2.py. Exercises the
# Filesystem-enum probe reaching its third arm (FAT32 + exFAT probes both fail on
# the ext2 partition, ext2 succeeds).
image-ext2: image $(EXT2_PART)
	python3 scripts/mkext2.py

# Boot the combined disk: UEFI boots BOOTAA64 from the FAT32 ESP (partition 2),
# then fsd mounts the ext2 partition (partition 1) read-only - so `ls`/`cat`/
# pipelines read from ext2 (their /bin binaries live there too).
run-image-ext2: image-ext2
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(EXT2_IMG),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-global virtio-mmio.force-legacy=false \
		-nographic

# `run-image` (real FAT32, so disk commands work) *and* the NIC from
# `run-net` in one boot - the fullest QEMU run: the filesystem server mounts,
# the shell/disk commands work, and init_net's boot-time ARP probe exercises
# the network. Every frame is dumped to $(NET_PCAP) for host-side inspection.
run-image-net: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev user,id=net0 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Like `run-image-net`, but forwards host port 5555 to the guest's TCP port 80
# (SLIRP hostfwd), so `netd`'s HTTP server (the guest answering the network) is
# reachable from the host: boot this, then on the host run
#   curl http://localhost:5555/
# and the from-scratch TCP stack serves its page. Every frame still goes to
# $(NET_PCAP) for host-side inspection.
run-image-server: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev user,id=net0,hostfwd=tcp::5555-:80 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# run-image-server plus a second hostfwd for netd's 9P-export listener (port
# 564): the cluster Phase 1 export gateway (docs/roadmap/roadmap-cluster-phase1.md). A
# host 9P client (scripts/np9p_client.py) reaches the guest's exported fsd via
#   python3 scripts/np9p_client.py localhost 5640 readdir /
# reading the guest's disk over TCP. curl http://localhost:5555/ still works.
run-image-9p: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev user,id=net0,hostfwd=tcp::5555-:80,hostfwd=tcp::5640-:564,hostfwd=tcp::5900-:9000 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# The remote-mount *client* side of cluster Phase 1 (step 1c): a NIC + a real
# FAT32 disk (so /bin's ls/cat load) and nothing else - the guest reaches a
# host-run 9P server at 10.0.2.2 over SLIRP with no hostfwd needed (SLIRP routes
# guest->host automatically). On the host, first start the test server:
#   python3 scripts/np9p_server.py 5641
# then in the guest:
#   mount -r 10.0.2.2:5641 /mnt/a
#   ls /mnt/a ; cat /mnt/a/HELLO.TXT
# reading the *host's* filesystem over TCP - the "aha", one VM, host as server.
run-image-9p-client: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev user,id=net0 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# The two-VM integration - the cluster Phase 1 "aha" (step 1d): two guests share
# an L2 link via QEMU socket networking (a virtual hub, no SLIRP), each with a
# distinct MAC that netd derives a distinct static IP from (:0a -> 10.0.2.10,
# :0b -> 10.0.2.11). Machine A *exports* its disk (netd's port-564 listener is
# always on); machine B *remote-mounts* A over the shared link. Run in two
# terminals - **A first** (it listens; B's socket connect fails if nothing is):
#   Terminal 1:  make run-image-2vm-a
#   Terminal 2:  make run-image-2vm-b
# then in B's shell:
#   mount -r 10.0.2.10:564 /mnt/a
#   ls /mnt/a            # A's disk: BIN/ EFI/
#   cat /mnt/a/EFI/ORBS/INIT.CFG
# Each VM gets its own disk copy (two QEMU write-locks can't share one file) and
# its own pcap (build/net-a.pcap / build/net-b.pcap) for tcpdump inspection.
# The FAT32 pair, with the same per-node identity split as the ext2 pair above.
# It would otherwise boot one image twice and give BOTH nodes node-a's private
# key while `authorized` says .11 is node-b - harmless while nothing reads the
# files, and a confusing authentication failure the moment step 7 lands.
images-2vm:
	rm -f $(ESP_DIR)-a.img $(ESP_DIR)-b.img $(ESP_DIR).img
	$(MAKE) image CLUSTER_NODE=node-a
	cp $(ESP_DIR).img $(ESP_DIR)-a.img
	rm -f $(ESP_DIR).img
	$(MAKE) image CLUSTER_NODE=node-b
	cp $(ESP_DIR).img $(ESP_DIR)-b.img
	@echo "images-2vm: built node-a and node-b FAT32 disks with distinct identities"

$(ESP_DIR)-a.img $(ESP_DIR)-b.img:
	@echo "$@ is missing - run 'make images-2vm' first."; \
	 echo "(The two nodes hold different keys and must be built together.)"; exit 1

run-image-2vm-a: $(ESP_DIR)-a.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR)-a.img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev socket,id=net0,listen=127.0.0.1:12340 \
		-device virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a \
		-object filter-dump,id=f0,netdev=net0,file=$(BUILD_DIR)/net-a.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

run-image-2vm-b: $(ESP_DIR)-b.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR)-b.img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev socket,id=net0,connect=127.0.0.1:12340 \
		-device virtio-net-device,netdev=net0,mac=52:54:00:12:34:0b \
		-object filter-dump,id=f0,netdev=net0,file=$(BUILD_DIR)/net-b.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

# The two-node cluster on EXT2 rather than FAT32 - the only rig that can show
# PERMISSIONS crossing the cluster, because FAT32 records no mode for fsd to
# enforce, so every remote request looks permitted there whatever identity it
# carries (a permission test on the FAT32 pair passes before a fix AND after
# it). Same MAC-derived IPs as that pair, but its OWN link port, so both rigs
# can be up at once instead of colliding as an unrelated guest-side timeout.
# Each node gets its own disk copy, since both write to it.
# The two nodes hold DIFFERENT private keys, so they can no longer be one image
# copied twice - each is a full rebuild with its own CLUSTER_NODE. The
# `authorized` file is identical in both (every node accepts the same peers);
# only `/etc/cluster/id` differs.
#
# BUILT BY ONE TARGET, SEQUENTIALLY, AND NEVER BY THE RUN TARGETS. Two reasons,
# both learned the hard way:
#
#  - Both builds pass through the SAME intermediates ($(EXT2_PART), $(EXT2_IMG),
#    build/ext2-src). The documented workflow runs node A in one terminal and
#    node B in another, and if each `make run-…` built its own image those two
#    invocations would interleave over those shared paths - B's `cp` could copy
#    the node-A image and both guests would silently boot as node-a, which is
#    the exact condition per-machine keys exist to prevent.
#  - A run target that rebuilds would also rewrite the disk a running QEMU has
#    open.
#
# So: `make images-2vm-ext2` builds the pair, and the run targets refuse if it
# has not been run. Re-run it after changing any source you want on the nodes -
# that is the cost of removing the race, and it is visible rather than silent.
images-2vm-ext2: esp
	rm -f $(BUILD_DIR)/espext2-a.img $(BUILD_DIR)/espext2-b.img $(EXT2_PART) $(EXT2_IMG)
	$(MAKE) image-ext2 CLUSTER_NODE=node-a
	cp $(EXT2_IMG) $(BUILD_DIR)/espext2-a.img
	rm -f $(EXT2_PART) $(EXT2_IMG)
	$(MAKE) image-ext2 CLUSTER_NODE=node-b
	cp $(EXT2_IMG) $(BUILD_DIR)/espext2-b.img
	@echo "images-2vm-ext2: built node-a and node-b disks with distinct identities"

# A missing per-node image is an instruction, not a build: see above for why
# these must not be produced by the run targets.
$(BUILD_DIR)/espext2-a.img $(BUILD_DIR)/espext2-b.img:
	@echo "$@ is missing - run 'make images-2vm-ext2' first."; \
	 echo "(The two nodes hold different keys and must be built together, once,"; \
	 echo " not by each run target - see the comment in the Makefile.)"; exit 1

run-image-2vm-ext2-a: $(BUILD_DIR)/espext2-a.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(BUILD_DIR)/espext2-a.img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev socket,id=net0,listen=127.0.0.1:12341 \
		-device virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a \
		-object filter-dump,id=f0,netdev=net0,file=$(BUILD_DIR)/net-ext2-a.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

run-image-2vm-ext2-b: $(BUILD_DIR)/espext2-b.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(BUILD_DIR)/espext2-b.img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-device virtio-rng-device \
		-netdev socket,id=net0,connect=127.0.0.1:12341 \
		-device virtio-net-device,netdev=net0,mac=52:54:00:12:34:0b \
		-object filter-dump,id=f0,netdev=net0,file=$(BUILD_DIR)/net-ext2-b.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Wraps build/esp.img into build/esp.hdd, a Parallels-native virtual hard disk, via
# prl_disk_tool's `--dmg` import (its only documented way to build a .hdd
# from an existing raw image; needs a real .dmg container, not the raw .img,
# and prl_disk_tool silently fails without absolute paths). Attach build/esp.hdd
# as the VM's Hard Disk device in Parallels - not build/esp.img, and not CD/DVD
# (that's for optical filesystems, and MBR+FAT32 isn't one).
#
# build/esp.hdd only stores a pointer to build/esp.dmg's absolute path, not a copy of
# its data - keep both files together, don't delete build/esp.dmg separately.
parallels-hdd: image
	rm -f $(ESP_DIR).dmg
	hdiutil convert $(ESP_DIR).img -format UDZO -o $(ESP_DIR).dmg
	rm -rf $(ESP_DIR).hdd
	"$(PDT)" create --hdd "$(CURDIR)/$(ESP_DIR).hdd" --dmg "$(CURDIR)/$(ESP_DIR).dmg"

# Stages the Raspberry Pi 4 / Pi 400 boot card: the pftf UEFI firmware
# (pinned, checksummed, cached under build/cache) plus build/esp on top, on an
# already-mounted FAT volume. It never formats; see scripts/sdcard.sh for the
# guards and docs/testing/testing-pi4.md section 4 for the card itself.
# The firmware is installed only on a card that has none, since its settings
# and UEFI variables live inside RPI_EFI.fd.
#
#   make sdcard SDCARD=/Volumes/OUROBOROS
#   make sdcard SDCARD=/Volumes/OUROBOROS KEEP_ETC=1   # keep the card's /etc
#   make sdcard SDCARD=/Volumes/OUROBOROS FIRMWARE=1   # reinstall the firmware
#   make sdcard SDCARD=/Volumes/OUROBOROS EJECT=1
# The SDCARD check runs before the build, so a missing or mistyped path fails
# in a second rather than after a full rebuild; the script checks it properly.
sdcard:
	@test -n "$(SDCARD)" && test -d "$(SDCARD)" || { \
		echo "sdcard: SDCARD='$(SDCARD)' is not a mounted card, e.g. make sdcard SDCARD=/Volumes/OUROBOROS"; exit 1; }
	$(MAKE) esp
	STICK="$(STICK_MODE)" SDCARD="$(SDCARD)" KEEP_ETC="$(KEEP_ETC)" FIRMWARE="$(FIRMWARE)" EJECT="$(EJECT)" \
		ESP_DIR="$(ESP_DIR)" CACHE_DIR="$(BUILD_DIR)/cache" ./scripts/sdcard.sh

# Stages the same tree on a USB stick, the Pi's only disk once the kernel
# runs (the SD slot has no driver after exit_boot_services, testing-pi4.md
# section 6): what `mount -a` mounts and where /bin, /etc and /man come
# from. The sdcard target with STICK_MODE=1, so one recipe and one script
# (scripts/sdcard.sh, STICK=1, whose header lists the differences): the same
# guards, no firmware and no EFI tree, FAT32 or exFAT on partition 1 of an
# MBR disk, and its /etc kept by default. It never formats; in Disk Utility,
# MS-DOS (FAT) with the Master Boot Record scheme gives FAT32 on a stick
# over 2 GB.
#
#   make stick STICK=/Volumes/STICK
#   make stick STICK=/Volumes/STICK KEEP_ETC=0   # re-stage the stick's /etc
#   make stick STICK=/Volumes/STICK EJECT=1
stick:
	@test -n "$(STICK)" && test -d "$(STICK)" || { \
		echo "stick: STICK='$(STICK)' is not a mounted stick, e.g. make stick STICK=/Volumes/STICK"; exit 1; }
	$(MAKE) sdcard SDCARD="$(STICK)" STICK_MODE=1

# Cut a release: build the release-profile disk images and package the
# downloadable artifacts (esp.img.zip + esp.hdd.zip + SHA256SUMS) under
# build/release/. Local and repeatable. The outward-facing publish step
# (tag + push + GitHub Release) is deliberately NOT a make target - run
# `scripts/release.sh publish` by hand. Version comes from ./VERSION.
# See docs/RELEASING.md.
release:
	scripts/release.sh build

# Scripted real-hardware test loop against Parallels - the manual "boot,
# watch the screen, type on a keyboard, report back" round trip every
# postmortem in docs/ paid wall-clock time for, now driven headlessly via
# prlctl (Parallels Desktop's own CLI - `man prlctl`): rebuilds build/esp.hdd,
# boots the registered VM named $(VM_NAME), types each ;-separated
# command in $(CMDS) through Parallels' own virtual keyboard
# (`prlctl send-key-event`, confirmed to land on the same
# xhci::poll_key interrupt-endpoint path a real physical USB keyboard
# does - see docs/postmortems/xhci-keyboard-postmortem.md), and saves a screenshot
# after each step instead of needing a human watching live. See
# scripts/test-parallels.sh for the full mechanics.
#
# Needs the VM already registered in Parallels with its Hard Disk device
# pointed at this repo's build/esp.hdd (see `parallels-hdd`'s own doc comment
# above for how that gets built and attached in the first place - this
# target only rebuilds the disk image, it doesn't create/register a VM).
#
#   make test-parallels
#   make test-parallels CMDS="help;echo hi;uptime"
#   make test-parallels VM_NAME="Ouroboros" BOOT_WAIT=15
test-parallels:
	VM_NAME="$(VM_NAME)" CMDS="$(CMDS)" BOOT_WAIT="$(BOOT_WAIT)" ./scripts/test-parallels.sh

# The keyboard-chain check: four driven boots of build/esp.img through the
# nested-shell recipes in docs/testing/testing-qemu.md section 1b, graded on
# the lines they name (scripts/test-keyboard-chain.sh). Minutes, not seconds,
# so it is not in `make test`; run it when tasks.rs's keyboard ownership
# changes. Depends on `image` so it can never grade a stale kernel (the
# script also refuses an esp.img older than the kernel binary).
test-keyboard-chain: image
	PROFILE="$(PROFILE)" ./scripts/test-keyboard-chain.sh

# The USB hub check (scripts/test-usb-hub.py): three driven boots with a
# keyboard and a storage stick behind a usb-hub, graded on outcomes (keyboard
# ready, stick configured, a line typed through the USB keyboard runs). The
# second, --usb-boot, boots build/esp.img FROM the stick behind the hub with
# no other disk, so it must also mount through the hub. The third, --stall,
# is --usb-boot from build/usb-hub-stall.img (`make image-stall`, the image
# with the MSDSTALL boot flag): QEMU's stick stalls on every seventh CBW
# (Bulk-OUT) and on a short CSW read for another (Bulk-IN), about 30 of
# each, and the bulk-endpoint recovery must carry the boot through. Its
# image is built just before that boot, so a failure there costs only that
# boot. `python3 scripts/test-usb-hub.py --direct` is the control, the same
# devices on root ports. About three minutes, so not in
# `make test`; run it when xhci.rs's port scan, device setup or storage
# recovery changes, or usb_msd.rs's retry. Every boot always runs (a failure
# in one must not hide the next's result); the target fails if any did.
test-usb-hub: image $(USBSTICK_IMG)
	@fail=0; inc=0; hard=0; \
	python3 scripts/test-usb-hub.py; s=$$?; [ $$s -eq 100 ] && inc=1; [ $$s -ne 0 ] && fail=1; [ $$s -ne 0 ] && [ $$s -ne 100 ] && hard=1; \
	python3 scripts/test-usb-hub.py --usb-boot; s=$$?; [ $$s -eq 100 ] && inc=1; [ $$s -ne 0 ] && fail=1; [ $$s -ne 0 ] && [ $$s -ne 100 ] && hard=1; \
	if $(MAKE) --no-print-directory image-stall; then \
		python3 scripts/test-usb-hub.py --stall; s=$$?; [ $$s -eq 100 ] && inc=1; [ $$s -ne 0 ] && fail=1; [ $$s -ne 0 ] && [ $$s -ne 100 ] && hard=1; \
	else fail=1; hard=1; fi; \
	if [ $$hard -eq 1 ]; then echo "test-usb-hub: a layout FAILED its checks"; \
	elif [ $$inc -eq 1 ]; then echo "test-usb-hub: a layout was INCONCLUSIVE (QEMU's firmware never loaded the kernel) and none failed, so this is not a pass: run it again"; fi; \
	exit $$fail

# The early fault reporter (kernel/src/earlyfault.rs) exercised on QEMU
# (scripts/test-early-fault.py): one boot with the EARLYFAULT flag file at the
# ESP root, where the kernel asks the firmware's CopyMem to write at an
# unmapped address just before the xHCI takeover, so the fault is taken in
# the firmware's own code with the firmware's vectors installed, as on the
# Pi 4; the serial console must show the reporter's dump (ESR decoded, the
# register rows before the first frame, the faulting PC placed in the DXE
# core by name, a frame placed in the kernel) and not the firmware's one
# line. A boot with EARLYFAULT and WALKFAULT, where the report faults inside
# its own image walk and must have printed the rows and then the one line a
# nested fault prints, with FAR and the image under read. Then a boot without
# a flag, which must reach the shell with the reporter armed and answer
# `help`. Every boot's QEMU trace is graded too. About ninety seconds. Run it
# when earlyfault.rs, the console discovery it prints through, or the
# kernel's frame-pointer setting (.cargo/config.toml) changes.
test-early-fault: esp
	python3 scripts/test-early-fault.py

# The EL1 drop (kernel/src/el2.rs, docs/roadmap/roadmap-el1-drop.md): the
# same three boots handed off at EL2 (virtualization=on, as the Raspberry
# Pi's firmware does). The control must say `running at EL2`, `dropped
# from EL2 to EL1`, reach the shell and answer `help` after a dwell, where
# before the drop the first eret into task 0, made at EL2, restored the
# firmware's stale ELR_EL2 and landed at address 0. About a minute; run it
# whenever el2.rs, mmu.rs's switch or
# exceptions::install changes.
test-el1-drop: esp
	python3 scripts/test-early-fault.py --el2

# The re-entrant session check (scripts/test-reentrant-session.sh): a remote
# fid op arriving at netd while it is inside a cpu run, on the two-node ext2
# rig. Minutes, so not in `make test`; run it when netd's client paths change.
test-reentrant-session: images-2vm-ext2
	./scripts/test-reentrant-session.sh

# The async remote-mount checks (docs/roadmap/roadmap-async-rmount.md): a
# driven guest against host-run 9P peers, served / identity / concurrent, each
# with a measured negative control. Three boots, minutes; not in `make test`.
test-async-rmount: image
	./scripts/test-async-rmount.sh

# The held-key checks (step 4 of docs/roadmap/roadmap-user-keys.md): a login
# holds its user's cluster key in netd, a logout or a killed login shell drops
# it, and an ordinary user can neither fill, list nor empty the table. Five
# driven boots of the FAT32 image, the table read from the host through the
# export; minutes, so not in `make test`. Run it whenever login, the shell's
# session loop or netd's held-key table changes.
test-held-keys: image
	python3 scripts/test-held-keys.py

# The user heap on a booted image: /bin/CMEM twice in one boot
# (scripts/test-heap.py). Each run checks its heap is at least 1 MiB, that
# malloc holds nearly all of it with every byte read back, and that the heap
# read 0 before its first malloc; the second run starts in the slot the first
# gave back, so it is the check on the loader zeroing a region. One boot, about
# a minute; run it whenever HEAP_PAGES, populate_region or the runtime region
# allocator changes.
test-heap: image
	python3 scripts/test-heap.py

# A task's FP/SIMD registers across the kernel: /bin/FPPROBE alone, then
# `fpprobe | fpprobe -` (scripts/test-fpsimd.py). All 32 vector registers and
# FPCR must survive a YIELD and a spin the tick preempts, with two probes
# holding different rounding modes side by side. One boot, about a minute; run
# it whenever exceptions.rs's trampolines, Context, or tasks.rs's switch paths
# change.
test-fpsimd: image
	python3 scripts/test-fpsimd.py

# cond's escape sequences on a framebuffer, read off the pixels
# (scripts/test-cond-vt.py): QEMU with -device ramfb, the screen taken by QMP
# screendump and decoded cell by cell with cond's own font, /bin/VTPROBE's
# screen compared with a model. One boot, under a minute; run it whenever
# cond's framebuffer backend, its font or the kernel's FB_* primitives change.
test-cond-vt: image
	python3 scripts/test-cond-vt.py

# The navigation and function keys on QEMU's USB keyboard
# (scripts/test-nav-keys.py): the eleven keys pressed by monitor sendkey, the
# bytes /bin/READKEY reads checked against their VT100 sequences, then the
# shell and login taking them as nothing. One boot, about a minute; run it
# whenever xhci.rs's key mapping, the shell's line editor or login's reader
# changes.
test-nav-keys: image
	python3 scripts/test-nav-keys.py

# argv in a C program (scripts/test-cargs.py, /bin/CARGS): three pairs of runs,
# the Rust /bin/ARGS and its C twin with the same arguments, which must print
# the same vector apart from argv[0]. One boot, about a minute; run it whenever
# libc/src/crt0.c or the kernel's argv store changes.
test-cargs: image
	python3 scripts/test-cargs.py

# errno from the C file layer (scripts/test-cerrno.py, /bin/CERRNO): two boots,
# FAT32 as root and ext2 as root and then as `user`, who must get EACCES for
# /etc/shadow and yet stat it; stat(path)'s checks are here too (step 4). About
# two minutes; run it whenever libc/src/file.c's error paths or stat, or fsd's
# answers to them, change.
test-cerrno: image image-ext2
	DEBUGFS="$(DEBUGFS)" python3 scripts/test-cerrno.py

# The environment in a C program (scripts/test-cenv.py, /bin/CENV and CENVH):
# after `set`, both C programs print /bin/PRINTENV's lines, and CENV's getenv
# answers a set name, an unset one and SOURCE_DATE_EPOCH; then /bin/RDPROBE
# reads all five per-task stores through a 4 KiB buffer. One boot, about a
# minute; run it whenever libc/src/crt0.c, the kernel's env store or its store
# reads (copy_out) change.
test-cenv: image
	python3 scripts/test-cenv.py

# The clock in a C program (scripts/test-cclock.py, /bin/CCLOCK): its own checks
# of time, gettimeofday and clock_gettime, its ctime against the host's
# formatting of the same second, and its clock against the host's over a timed
# gap between two runs. One boot, about a minute; run it whenever
# libc/pico/clock.c or the kernel's MONOTONIC_US changes.
test-cclock: image
	python3 scripts/test-cclock.py

# The C headers on the disk (scripts/test-include.py): on FAT32, ext2 and
# exFAT, every staged file is there by path with its size; each directory
# small enough for one NP_READDIR reply lists exactly build/esp/include's names
# and sizes, and each larger one lists only staged names, in their case;
# three files cat to the host's bytes; and names made on the guest keep their
# case, on all three (FAT32 by byte 12's case flags). Three boots, about five minutes; run it whenever the header
# staging, or a filesystem's directory listing or naming, changes.
test-include: image image-ext2 image-exfat
	python3 scripts/test-include.py

# The C preprocessor on Ouroboros, the C-hosting plan's finish line
# (scripts/test-cpp.py): `cpp -o` of hello.c, CPICO's picodemo.c and a
# high-byte input, with no options, on a copy of the FAT32 image; the host
# reads the .i files back off it and compares them byte for byte with the same
# cpp built for the Mac, with signed and with unsigned char, and clang for this
# target compiles them. One boot, about a minute and a half;
# needs DevTools's cpp at CPP_DIR. Run it whenever the C runtime, the header
# stage or cpp-bin changes.
# Without cpp's sources there is nothing to test: test-cpp sets
# NEED_CPP=required (above), which stops before the image is built, not after.
test-cpp: image
	CPP_DIR="$(CPP_DIR)" CFLAGS_OS="$(CFLAGS_OS)" PICO_INC="$(PICO_INC)" python3 scripts/test-cpp.py

# `unmount` with a partition mount and an open file, on a copy of the ext2
# image (scripts/test-unmount.py): both trees cleared, a held fid answering
# NO_FS and never handed to the next opener, the shell's partition binding
# dropped, and `erase` allowed after. One boot, about a minute and a half; run
# it whenever FSOP_UNMOUNT, the fid table or the shell's mount code changes.
test-unmount: image-ext2
	python3 scripts/test-unmount.py

# unlink, rename and fstat in the C port (scripts/test-crename.py, /bin/CRENAME
# and /bin/CFSTAT): two boots, FAT32 and ext2 (with a partition at /mnt/f for
# EXDEV), the checks Proem's and Edit's handoff notes name. About two
# minutes; run it whenever libc's unlink/rename/fstat, its headers, or fsd's
# NP_RM/NP_MV/NP_FSTAT changes.
test-crename: image image-ext2
	python3 scripts/test-crename.py

# Host unit tests for the PURE crates - the ones with no I/O, no syscalls and no
# target dependency, so they run natively on the build machine. This exists
# because a pure crate can otherwise have NO build coverage at all: it is a
# workspace member but not a default-member, and until something depends on it a
# bare `cargo build`, `make build` and `make esp` all stay green while it is
# broken. `ed25519` is exactly that case for the length of its arc, which is
# several PRs.
#
# Native target, not the workspace default (aarch64-unknown-uefi), which cannot
# run a test binary.
HOST_TARGET := $(shell rustc -vV | sed -n 's/^host: //p')
PURE_CRATES := accounts regex ed25519 clusterkeys ninep-abi
# `ninep-abi` joined the list at the flag day. It had always qualified - pure
# consts plus `resolve_ns`, no I/O and no syscalls - and was simply never listed,
# so its thirteen assertions about the WIRE FORMAT, including the one pinning
# `NP_FRAME_MAX`, were compiled by nothing at all. That is precisely the failure
# this variable's comment above describes, sitting on the crate that defines the
# format both Python peers transcribe from.

# The PIE relocation contract, checked mechanically over every userland binary:
# ABS64 is unloadable by this project's loader. See scripts/check-relocs.sh for
# why it also fails when the tool reports NO relocations at all.
check-relocs:
	@./scripts/check-relocs.sh

# The xHCI driver's two DMA barriers in the built kernel (2026-10-03). No QEMU
# rig can fail for a missing one: QEMU's controller sees guest memory as the
# CPU last wrote it, and only the Pi's non-coherent PCIe controller needs
# them. xhci.rs keeps each barrier and its store as a naked function of fixed
# instructions, and the script finds each in the image, once, and a call to it.
# Shown to fail by deleting a barrier, putting a `nop` before the store, and
# routing write32 itself around its function. It does NOT see one new raw
# register write added beside the others (see the script's docstring). PART
# OF `test`, since a check outside the default suite decays; checks the
# PROFILE that `make sdcard` stages (debug unless PROFILE says otherwise).
check-xhci-barriers: build
	@python3 scripts/check-xhci-barriers.py $(KERNEL) $(dir $(OBJCOPY))

# The published website vs the documents it abridges. docs/ is served live by
# GitHub Pages and docs/site/*.html is hand-written - an abridgement, not a
# rendering - so nothing notices when a source moves on and its page does not,
# and the stale answer is the PUBLIC one. See the script's module docstring for
# why it compares blob hashes rather than commit dates.
#
# PART OF `test` since 2026-09-05, which is the whole point of building it: a
# check outside the default suite decays. It was held out at first because nine
# pages were already behind (the site froze on 2026-08-23) and a permanently-red
# suite trains everyone to ignore it. That backlog was cleared by re-abridging
# five pages and DELETING the other four - changelog, roadmap, shell-commands
# and processes are reference material that wants to be current rather than
# abridged, so docs.html links the markdown on GitHub and there is no copy left
# to drift. Fewer pages fixed than were broken, on purpose.
check-site:
	@python3 scripts/check-site-freshness.py

test:
	@for c in $(PURE_CRATES); do \
		echo "== cargo test -p $$c"; \
		cargo test -p $$c --target $(HOST_TARGET) || exit 1; \
	done
	@# Lint the TESTS too. Plain `cargo clippy -p <crate>` does not build test
	@# targets, so a warning inside a #[cfg(test)] module (a dead fixture
	@# constant, say) goes unreported - and "clippy clean" then means less than
	@# it sounds like. This has been claimed too broadly twice, so it is checked
	@# here rather than remembered.
	@for c in $(PURE_CRATES); do \
		cargo clippy -p $$c --all-targets --target $(HOST_TARGET) -- -D warnings || exit 1; \
	done
	@# The protocol constants are spelled independently in Rust and in the two
	@# Python peers - there is no shared header across that boundary - so they
	@# can drift, and a drift surfaces as "authentication failed", which reads
	@# as a key problem rather than a constant problem.
	@echo "== cross-language wire constants"
	@python3 scripts/check-wire-constants.py || exit 1
	@# The host 9P peer's docstring lists which verbs it serves, and nothing
	@# compared that prose to its dispatch chain - so when a range test silently
	@# swallowed the five fid verbs, the prose went on claiming otherwise. This
	@# drives one request per verb and checks the group each lands in.
	@echo "== 9P peer verb dispatch"
	@python3 scripts/np9p_server.py --self-test || exit 1
	@# The PBKDF2 vectors are pasted from a hashlib run into ed25519 and, two of
	@# them, into edtest. A paste nobody re-checks is a copy that drifts, and the
	@# vectors are what stand between the crate and a key every node derives.
	@echo "== PBKDF2 vectors vs hashlib"
	@python3 scripts/gen-pbkdf2-vectors.py --check || exit 1
	@# The published site abridges documents in this repo, and nothing lays the
	@# two side by side - not the compiler, not a test, not review, because the
	@# drift crosses a FILE FORMAT. The failure is silent and outward-facing: a
	@# public page keeps serving a confident, stale answer. See check-site above.
	@echo "== published site vs its sources"
	@python3 scripts/check-site-freshness.py || exit 1
	@# test-usb-hub reports one firmware stall as INCONCLUSIVE rather than as
	@# failed checks; a classifier that grew too broad would turn a kernel
	@# that never starts into that verdict, so it is checked on fixed
	@# transcripts here, where it runs in a second.
	@echo "== test-usb-hub's INCONCLUSIVE classifier"
	@python3 scripts/test-usb-hub.py --self-test || exit 1
	@echo "== xHCI barriers in the kernel image"
	@$(MAKE) --no-print-directory check-xhci-barriers || exit 1
	@echo "== all pure-crate host tests passed, and clippy clean including tests"

clean:
	cargo clean
	rm -rf $(BUILD_DIR)
