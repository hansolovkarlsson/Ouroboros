//! Loads a userland program from the ESP into an EL0-accessible buffer,
//! entirely during the UEFI boot-services window (before
//! `exit_boot_services`) — there is no runtime disk driver yet, and won't
//! be for a while; see `docs/processes.md` for the full design and why
//! this is a deliberate, documented shortcut rather than a placeholder for
//! "fix later" carelessness.
//!
//! Which program to load is read from a tiny one-line config file rather
//! than hardcoded, so the shell (or, eventually, whatever else gets loaded
//! this way) can be swapped by editing a file on the ESP, no kernel
//! rebuild required — the actual "configuration" that milestone was about.
//!
//! ## A real ELF loader now, not a flat-binary `memcpy` — the relocating
//! ## loader milestone
//!
//! Every userland program used to be a flat, position-*dependent* binary:
//! linked at a fixed base of `0x0`, copied byte-for-byte to wherever
//! `boot::allocate_pages` happened to place it, with no relocation step to
//! fix the mismatch. That was this project's single most-repeated bug
//! class — `core::fmt`'s argument-dispatch table and slice/literal
//! comparisons both crashed for the identical reason (a data *pointer*
//! baked in for the wrong base address) — previously avoided only by
//! hand-discipline ("don't do that"), not actually fixed. This module now
//! parses real ELF64: walks `PT_LOAD` program headers to know what to copy
//! where, and processes `R_AARCH64_RELATIVE` self-relocations in
//! `.rela.dyn` against wherever the program actually landed.
//!
//! Hand-rolled, not a crate — matching this project's established
//! discipline (`acpi.rs`/`madt.rs`/`fat32.rs` all hand-roll their formats
//! rather than pull in a library, since each is simple enough on its own
//! terms to justify it). ELF64's header, program headers, section headers,
//! and `Elf64_Rela` entries are all small, fixed-offset structs — the same
//! category of "simple enough" this project has repeatedly judged in favor
//! of hand-rolling. This module runs entirely before `exit_boot_services`,
//! so `alloc` is fully available here (unlike `fat32.rs`) — a real,
//! worth-noting difference in constraints from every other hand-rolled
//! parser in this codebase, though this parser doesn't lean on it much.
//!
//! **Deliberately narrow in scope, matching this project's "narrowest
//! useful case" discipline:** only `R_AARCH64_RELATIVE` self-relocations
//! are supported — this project has no shared libraries and no imported
//! symbols to resolve, so no other relocation type should ever
//! legitimately appear, and one does show up unexpectedly, that's a hard
//! error, not a silently-ignored one. No W^X segment permission
//! separation (still one combined RWX EL0 region, same as before this
//! milestone — a separately-tracked, pre-existing weakness this work
//! doesn't attempt to solve). No dynamic linking, no `PT_INTERP` support
//! (the build passes `--no-dynamic-linker` for exactly this reason).
//!
//! **A real, hard-won toolchain constraint, confirmed by direct
//! experiment, not guessed:** userland programs must be built in
//! `--release`, not debug. A debug build fails to *link* at all under the
//! new position-independent model — `rust-lld` rejects an
//! `R_AARCH64_ABS64` relocation inside
//! `core::fmt::builders::PadAdapter`, pulled in from the prebuilt (not
//! rebuilt-per-project — see `rust-toolchain.toml`) `libcore.rlib` by
//! ordinary debug-build panic/bounds-check formatting machinery, which
//! that prebuilt rlib was never compiled with `-C relocation-model=pic`
//! support for. A release build's optimizer eliminates enough of that
//! code that the poisoned object code never gets pulled into the link at
//! all. See the Makefile's `SHELL_ELF`/`shell-bin` comments for where
//! this is actually enforced. Not something `-Z build-std` would be worth
//! reintroducing a nightly-toolchain dependency to work around (see
//! `rust-toolchain.toml`'s own "no nightly, no `-Z build-std` needed").
//!
//! ## Why the loaded region is over-allocated and trimmed to a 2MB boundary
//!
//! `mmu.rs` gives a task's EL0 region fine-grained (4KB page) access by
//! splitting exactly one 2MB L2 slot into an L3 sub-table. That only works
//! if the whole region fits inside a single 2MB-aligned slot — true by
//! construction for the old compile-time `#[repr(align(N))]` EL0 statics,
//! but `boot::allocate_pages` only guarantees 4KB (page) alignment, so a
//! multi-page region loaded this way could easily straddle a 2MB boundary
//! depending on where the allocator happens to put it. Rather than
//! generalizing `mmu.rs` to handle a region spanning multiple L2 slots,
//! [`load`] asks for `size + 2MB` worth of pages, then frees whatever
//! falls before/after the first 2MB-aligned address inside that range —
//! guaranteeing the final region can't straddle a slot, the same
//! invariant the old static got for free from its alignment. Costs up to
//! just under 2MB of transiently-allocated-then-freed memory at boot; RAM
//! is abundant enough (512MB in the QEMU config) for this to not matter.
//! Unaffected by the switch to real ELF loading - this trick only ever
//! cared about the *total* region size, however that size gets derived.

use alloc::string::String;
use core::mem::size_of;
use core::ptr::NonNull;

use uefi::boot::{self, AllocateType, MemoryType, PAGE_SIZE};
use uefi::fs::FileSystem;
use uefi::CString16;

/// Fixed for now, not itself configurable - has to live somewhere the
/// kernel knows to look without already having read a config file to find
/// it. Everything downstream of this one path is configuration-driven.
///
/// `ORBS`, not `OUROBOROS` - the full project name is 9 characters, one
/// over FAT's 8.3 short-name limit. Real FAT32 formatters store any
/// longer name in a long-filename (LFN) entry alongside a mangled 8.3
/// alias (`OUROBO~2`) for compatibility, and `fat32.rs`'s runtime reader
/// doesn't parse LFN entries - so a 9-letter directory name here would
/// make this same path unreachable by the shell's own `cd`/`ls`. This
/// project controls the name, so it's the name that gives, not a parser
/// feature added just to accommodate one avoidable 9-letter directory.
/// (The truncation was first `OUROBORO` - exactly 8 characters - then
/// renamed to the tidier `ORBS` abbreviation.)
const CONFIG_PATH: &str = "\\EFI\\ORBS\\INIT.CFG";

// 16KB. Was 8KB (2 pages) - but the stack guard page (below) immediately
// caught the shell's own `exec` path overflowing 8KB by ~32 bytes
// (`cmd_exec` -> `spawn_path` -> `fs_call`'s 768+768-byte request/reply
// buffers + staging + the call chain), a real, silent, pre-existing
// overflow into the top of the code region that had gone unnoticed because
// nothing was mapped to fault on it. Grown to 16KB to give that path
// comfortable headroom; if `mmu.rs`'s `guard_page_addr` `STACK_PAGES` isn't
// kept equal to this, the guard lands in the wrong place.
//
// Grown again to 24KB (6 pages) once the network server (`netd`) appeared:
// its client ops (ping/resolve/fetch) and TCP server nest several 1600/2048
// -byte frame buffers down one call chain (serve -> handle_* -> arp/tcp/dns),
// sitting right at the 16KB edge - a later change (the flow-control drain
// loop) tipped `ping` over it into the guard page (a clean fault, caught, not
// silent - exactly what the guard is for). netd is the most stack-hungry
// program; 24KB gave it margin.
//
// Grown again to 32KB (8 pages) when netd gained concurrent connections: its
// serve() frame now permanently holds MAX_CONNS TcpConns (each ~2.3KB, a 2KB
// response-prefix buffer dominating), and a client op (ping/resolve/fetch)
// nests its own ~5KB TCP/DNS buffers on top of that - which overflowed 24KB
// (caught, again, by the guard). All regions grow 8KB; RAM is ample.
const STACK_PAGES: u64 = 8;
/// One inaccessible guard page between the code and the stack. The stack
/// grows down from the top of the region; an overflow past the 8KB stack
/// lands in this page, which `mmu.rs` maps EL1-only, taking a clean EL0
/// fault instead of silently corrupting the code below. See `mmu.rs`'s
/// `build_view` (which derives the guard's address from the region's
/// `(base, size)` and this same layout convention) and the stack-guard
/// milestone writeup.
const GUARD_PAGES: u64 = 1;
/// A fixed heap allowance per program, between the code and the guard page
/// (`[code][heap][guard][stack]`). Programs can't use `alloc`'s
/// collections - prebuilt `liballoc` has `R_AARCH64_ABS64` relocations a
/// PIE link rejects, and rebuilding it (`-Z build-std`) is nightly-only,
/// off-limits on this stable-only project - so this is a raw buffer a
/// program reads/writes via a `&mut [u8]` (reported by the `heap_info`
/// syscall), not a `GlobalAlloc`-backed heap. It lets a program hold data
/// far larger than its 16KB stack: the shell backs its redirect/pipe
/// capture with it, so `cat big > file` captures the whole file instead of
/// refusing. 256KB, still far inside one 2MB slot.
const HEAP_PAGES: u64 = 64;
const SLOT_ALIGN: u64 = 0x20_0000; // 2MB - see module doc comment.

/// The heap area `(base, size)` inside a loaded program's region, computed
/// from the region's `(base, size)` and the fixed `[code][heap][guard]
/// [stack]` layout: the heap sits `HEAP_PAGES + GUARD_PAGES + STACK_PAGES`
/// pages down from the region end, and is `HEAP_PAGES` long. Returns
/// `(0, 0)` for a region too small to have a heap (the idle task's single
/// page). Used by the `heap_info` syscall.
pub(crate) fn heap_area(base: u64, size: u64) -> (u64, u64) {
    let page = PAGE_SIZE as u64;
    let tail = (HEAP_PAGES + GUARD_PAGES + STACK_PAGES) * page;
    if size < tail {
        return (0, 0);
    }
    (base + size - tail, HEAP_PAGES * page)
}

/// Generous headroom, not a real limit this project has ever come close
/// to: the default shell has 6 program headers (3 `PT_LOAD` plus
/// `PT_DYNAMIC`/`PT_GNU_RELRO`/`PT_GNU_STACK`, confirmed directly via
/// `llvm-readobj -l` during the relocating-loader milestone).
///
/// A fixed array, not `Vec` - `parse_program_headers` runs from both the
/// boot-services-only path (`load()`, where `alloc` is fine) and the
/// runtime `spawn` syscall path (`syscall.rs`, long after
/// `exit_boot_services` has made the global allocator permanently
/// invalid). This distinction is a real bug this project found by
/// testing, not by review: the first real test of runtime `spawn` hung
/// silently, no exception and no abort, isolated via temporary debug
/// prints to a `Vec` allocation inside what's now this function - the
/// boot-services-backed global allocator doesn't fail cleanly once boot
/// services are gone, it just hangs.
const MAX_PROGRAM_HEADERS: usize = 16;

// ELF64 constants - cross-checked against the AArch64 ELF psABI and
// generic ELF64 spec, not transcribed from memory alone: `e_machine`
// EM_AARCH64 = 183 and `R_AARCH64_RELATIVE` = 1027 are the two most
// easily-mistyped values here, both independently confirmed against this
// project's own build output during scoping (`llvm-readobj -r`/`-h`
// showed exactly these, by name, for a real linked binary).
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_DYN: u16 = 3;
const EM_AARCH64: u16 = 183;
const PT_LOAD: u32 = 1;
const R_AARCH64_RELATIVE: u32 = 1027;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ElfHeader {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ProgramHeader {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SectionHeader {
    sh_name: u32,
    sh_type: u32,
    sh_flags: u64,
    sh_addr: u64,
    sh_offset: u64,
    sh_size: u64,
    sh_link: u32,
    sh_info: u32,
    sh_addralign: u64,
    sh_entsize: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rela {
    r_offset: u64,
    r_info: u64,
    r_addend: i64,
}

#[derive(Debug)]
pub enum LoaderError {
    Protocol(uefi::Error),
    Config(uefi::fs::Error),
    ConfigNotUtf8,
    ConfigEmpty,
    PathEncoding,
    Program(uefi::fs::Error),
    ProgramEmpty,
    /// [`FSD_PATH`] couldn't be read - kept distinct from [`Program`]
    /// so the boot log doesn't misleadingly blame `INIT.CFG` for a
    /// missing filesystem server.
    Fsd(uefi::fs::Error),
    /// [`CON_PATH`] couldn't be read - kept distinct from [`Program`]/
    /// [`Fsd`] so a missing console server is named for what it is.
    Cond(uefi::fs::Error),
    /// [`NET_PATH`] couldn't be read - kept distinct so a missing network
    /// server is named for what it is.
    Netd(uefi::fs::Error),
    Alloc(uefi::Error),
    /// File is smaller than a bare ELF header, or a header/table field
    /// points past the end of the file - either a corrupt/truncated file
    /// on the ESP, or (more likely, given this project builds every
    /// program itself) a real bug in this parser's own offset math.
    Truncated,
    BadMagic,
    UnsupportedClass(u8),
    UnsupportedEndian(u8),
    /// Expected `ET_DYN` (a real PIE, per this project's own build flags in
    /// `.cargo/config.toml`) and got something else - almost certainly
    /// means a program on the ESP wasn't built with this project's own
    /// toolchain settings.
    UnexpectedElfType(u16),
    WrongMachine(u16),
    /// More program headers than [`MAX_PROGRAM_HEADERS`] - see its own
    /// doc comment for why this is a fixed bound, not a `Vec`.
    TooManyProgramHeaders,
    NoLoadSegments,
    /// A relocation type other than `R_AARCH64_RELATIVE` showed up - this
    /// project has no imported symbols to resolve, so nothing else should
    /// ever legitimately appear here. Deliberately a hard error, not a
    /// silently-skipped one.
    UnsupportedRelocation(u32),
}

impl core::fmt::Display for LoaderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoaderError::Protocol(e) => write!(f, "couldn't open the boot volume: {e}"),
            LoaderError::Config(e) => write!(f, "couldn't read {CONFIG_PATH}: {e}"),
            LoaderError::ConfigNotUtf8 => write!(f, "{CONFIG_PATH} is not valid UTF-8"),
            LoaderError::ConfigEmpty => write!(f, "{CONFIG_PATH} is empty"),
            LoaderError::PathEncoding => write!(f, "program path isn't valid for a UEFI file path"),
            LoaderError::Program(e) => write!(f, "couldn't read the program named in {CONFIG_PATH}: {e}"),
            LoaderError::Fsd(e) => write!(f, "couldn't read {FSD_PATH}: {e}"),
            LoaderError::Cond(e) => write!(f, "couldn't read {CON_PATH}: {e}"),
            LoaderError::Netd(e) => write!(f, "couldn't read {NET_PATH}: {e}"),
            LoaderError::ProgramEmpty => write!(f, "program named in {CONFIG_PATH} is empty"),
            LoaderError::Alloc(e) => write!(f, "couldn't allocate memory for the program: {e}"),
            LoaderError::Truncated => write!(f, "program file is truncated or has an invalid ELF header/table offset"),
            LoaderError::BadMagic => write!(f, "program file isn't an ELF file (bad magic)"),
            LoaderError::UnsupportedClass(c) => write!(f, "unsupported ELF class {c} (expected ELFCLASS64)"),
            LoaderError::UnsupportedEndian(e) => write!(f, "unsupported ELF byte order {e} (expected little-endian)"),
            LoaderError::UnexpectedElfType(t) => write!(f, "unexpected ELF type {t} (expected ET_DYN - built with this project's own toolchain settings?)"),
            LoaderError::WrongMachine(m) => write!(f, "unexpected ELF machine {m} (expected EM_AARCH64)"),
            LoaderError::TooManyProgramHeaders => write!(f, "program has more than {MAX_PROGRAM_HEADERS} program headers"),
            LoaderError::NoLoadSegments => write!(f, "program has no PT_LOAD segments"),
            LoaderError::UnsupportedRelocation(t) => write!(f, "unsupported relocation type {t} (only R_AARCH64_RELATIVE is supported - no dynamic linking)"),
        }
    }
}

/// Where a loaded program ended up, how big its whole EL0-accessible
/// region is (segments, at the base, followed by whatever padding then a
/// stack growing down from the top - see `tasks.rs`'s use of this), and
/// its real entry point.
pub struct LoadedProgram {
    pub base: u64,
    pub size: u64,
    /// The address to set `ELR_EL1` to - `base + e_entry`. Happens to
    /// always equal `base` today (`programs/linker.ld` deliberately keeps
    /// `_start` at output offset 0, via `KEEP(*(.text.start))`), but
    /// callers (`tasks.rs`) should use this field, not assume that
    /// equality themselves - a real ELF's entry point doesn't have to be
    /// its first loaded byte in general.
    pub entry: u64,
}

/// Reads [`CONFIG_PATH`] for a program path, then reads and loads that
/// program. Must run before `exit_boot_services` - both the filesystem
/// read and the page allocation are boot services.
pub fn load() -> Result<LoadedProgram, LoaderError> {
    let fs_proto = boot::get_image_file_system(boot::image_handle()).map_err(LoaderError::Protocol)?;
    let mut fs = FileSystem::new(fs_proto);

    let config_path = CString16::try_from(CONFIG_PATH).unwrap();
    let config_bytes = fs.read(config_path.as_ref()).map_err(LoaderError::Config)?;
    let program_path = parse_config(&config_bytes)?;

    let program_path = CString16::try_from(program_path.as_str()).map_err(|_| LoaderError::PathEncoding)?;
    let program_bytes = fs.read(program_path.as_ref()).map_err(LoaderError::Program)?;
    if program_bytes.is_empty() {
        return Err(LoaderError::ProgramEmpty);
    }

    load_elf_into_el0_region(&program_bytes)
}

/// The filesystem server's fixed path - not configurable via
/// [`CONFIG_PATH`], deliberately: `INIT.CFG` names the *interactive*
/// program (task 0), while the FS server is infrastructure with a
/// fixed task slot (`syscall_abi::FSD_TASK`) that clients hardcode.
const FSD_PATH: &str = "\\EFI\\ORBS\\FSD.BIN";

/// Loads the filesystem server ([`FSD_PATH`]) the same way [`load`]
/// loads task 0's program. A missing/broken FSD.BIN is not fatal (the
/// caller logs and boots on without a filesystem - every FS request
/// then fails with "no such task", which the shell maps to its
/// no-filesystem message), unlike task 0's program, which the boot
/// genuinely can't proceed without.
pub fn load_fsd() -> Result<LoadedProgram, LoaderError> {
    let fs_proto = boot::get_image_file_system(boot::image_handle()).map_err(LoaderError::Protocol)?;
    let mut fs = FileSystem::new(fs_proto);
    let path = CString16::try_from(FSD_PATH).unwrap();
    let program_bytes = fs.read(path.as_ref()).map_err(LoaderError::Fsd)?;
    if program_bytes.is_empty() {
        return Err(LoaderError::ProgramEmpty);
    }
    // Register the raw image with the supervisor for crash/wedge
    // recovery - the kernel can restart a faulted or wedged filesystem
    // server from this copy without needing a filesystem to load it from
    // (see supervisor::restart). A too-big image just means no restart
    // capability, logged, not a failure.
    // Report WHY: a full registry and an oversized image are different
    // problems with different fixes, and naming the wrong one sends the
    // reader to measure a binary that was never too big.
    if let Some(why) = crate::supervisor::register(syscall_abi::FSD_TASK as usize, &program_bytes).why()
    {
        log::warn!(
            "Ouroboros kernel: FSD.BIN - {} - the server won't be restartable this boot",
            why
        );
    }
    load_elf_into_el0_region(&program_bytes)
}

/// The console server's fixed path - not configurable via [`CONFIG_PATH`]
/// for the same reason as [`FSD_PATH`]: the console server is
/// infrastructure with a fixed task slot (`syscall_abi::CON_TASK`) that
/// clients hardcode, not the interactive program `INIT.CFG` names.
const CON_PATH: &str = "\\EFI\\ORBS\\COND.BIN";

/// Loads the console server ([`CON_PATH`]) the same way [`load_fsd`]
/// loads the filesystem server, and registers it with the supervisor for
/// crash/wedge recovery too. A missing/broken COND.BIN is not fatal (the
/// caller logs and boots on with the kernel's own console handling all
/// output), unlike task 0's program.
pub fn load_cond() -> Result<LoadedProgram, LoaderError> {
    let fs_proto = boot::get_image_file_system(boot::image_handle()).map_err(LoaderError::Protocol)?;
    let mut fs = FileSystem::new(fs_proto);
    let path = CString16::try_from(CON_PATH).unwrap();
    let program_bytes = fs.read(path.as_ref()).map_err(LoaderError::Cond)?;
    if program_bytes.is_empty() {
        return Err(LoaderError::ProgramEmpty);
    }
    // Report WHY: a full registry and an oversized image are different
    // problems with different fixes, and naming the wrong one sends the
    // reader to measure a binary that was never too big.
    if let Some(why) = crate::supervisor::register(syscall_abi::CON_TASK as usize, &program_bytes).why()
    {
        log::warn!(
            "Ouroboros kernel: COND.BIN - {} - the console server won't be restartable this boot",
            why
        );
    }
    load_elf_into_el0_region(&program_bytes)
}

const NET_PATH: &str = "\\EFI\\ORBS\\NETD.BIN";

/// Loads the network server ([`NET_PATH`]) the same way [`load_cond`] loads
/// the console server, and registers it with the supervisor for crash/wedge
/// recovery too. A missing/broken NETD.BIN is not fatal (the caller logs and
/// boots on with no network - `ping` reports no server).
pub fn load_netd() -> Result<LoadedProgram, LoaderError> {
    let fs_proto = boot::get_image_file_system(boot::image_handle()).map_err(LoaderError::Protocol)?;
    let mut fs = FileSystem::new(fs_proto);
    let path = CString16::try_from(NET_PATH).unwrap();
    let program_bytes = fs.read(path.as_ref()).map_err(LoaderError::Netd)?;
    if program_bytes.is_empty() {
        return Err(LoaderError::ProgramEmpty);
    }
    // Report WHY: a full registry and an oversized image are different
    // problems with different fixes, and naming the wrong one sends the
    // reader to measure a binary that was never too big.
    if let Some(why) = crate::supervisor::register(syscall_abi::NET_TASK as usize, &program_bytes).why()
    {
        log::warn!(
            "Ouroboros kernel: NETD.BIN - {} - the network server won't be restartable this boot",
            why
        );
    }
    load_elf_into_el0_region(&program_bytes)
}

/// The config file is deliberately just one line naming the program path -
/// no key/value syntax, no comments, nothing to parse beyond "trim
/// whitespace". Grows a real format only if something actually needs a
/// second setting.
fn parse_config(bytes: &[u8]) -> Result<String, LoaderError> {
    let text = core::str::from_utf8(bytes).map_err(|_| LoaderError::ConfigNotUtf8)?;
    let path = text.trim();
    if path.is_empty() {
        return Err(LoaderError::ConfigEmpty);
    }
    Ok(String::from(path))
}

/// Reads a `T` out of `file` at byte offset `offset`, bounds-checked
/// against the file's length first - every fixed-size ELF struct in this
/// module goes through this, so a truncated/corrupt file is always a
/// clean [`LoaderError::Truncated`], never an out-of-bounds read.
fn read_at<T: Copy>(file: &[u8], offset: usize) -> Result<T, LoaderError> {
    let end = offset.checked_add(size_of::<T>()).ok_or(LoaderError::Truncated)?;
    if end > file.len() {
        return Err(LoaderError::Truncated);
    }
    Ok(unsafe { core::ptr::read_unaligned(file[offset..].as_ptr().cast::<T>()) })
}

fn parse_elf_header(file: &[u8]) -> Result<ElfHeader, LoaderError> {
    let header: ElfHeader = read_at(file, 0)?;
    if header.e_ident[0..4] != ELF_MAGIC {
        return Err(LoaderError::BadMagic);
    }
    if header.e_ident[4] != ELFCLASS64 {
        return Err(LoaderError::UnsupportedClass(header.e_ident[4]));
    }
    if header.e_ident[5] != ELFDATA2LSB {
        return Err(LoaderError::UnsupportedEndian(header.e_ident[5]));
    }
    if header.e_type != ET_DYN {
        return Err(LoaderError::UnexpectedElfType(header.e_type));
    }
    if header.e_machine != EM_AARCH64 {
        return Err(LoaderError::WrongMachine(header.e_machine));
    }
    Ok(header)
}

/// A fixed-capacity stand-in for `Vec<ProgramHeader>` - see
/// [`MAX_PROGRAM_HEADERS`]'s doc comment for why this can't be a real
/// `Vec`. `Clone`/`Copy` since it's small (16 * 56 bytes) and both
/// callers of [`parse_program_headers`] just want to pass the populated
/// prefix around as a slice.
#[derive(Clone, Copy)]
pub(crate) struct ProgramHeaders {
    headers: [ProgramHeader; MAX_PROGRAM_HEADERS],
    count: usize,
}

impl ProgramHeaders {
    pub(crate) fn as_slice(&self) -> &[ProgramHeader] {
        &self.headers[..self.count]
    }
}

fn parse_program_headers(file: &[u8], header: &ElfHeader) -> Result<ProgramHeaders, LoaderError> {
    let count = header.e_phnum as usize;
    if count > MAX_PROGRAM_HEADERS {
        return Err(LoaderError::TooManyProgramHeaders);
    }
    let zero = ProgramHeader { p_type: 0, p_flags: 0, p_offset: 0, p_vaddr: 0, p_paddr: 0, p_filesz: 0, p_memsz: 0, p_align: 0 };
    let mut headers = [zero; MAX_PROGRAM_HEADERS];
    for (i, slot) in headers.iter_mut().enumerate().take(count) {
        let offset = header.e_phoff as usize + i * header.e_phentsize as usize;
        *slot = read_at::<ProgramHeader>(file, offset)?;
    }
    Ok(ProgramHeaders { headers, count })
}

fn read_section_header(file: &[u8], header: &ElfHeader, index: u16) -> Result<SectionHeader, LoaderError> {
    let offset = header.e_shoff as usize + index as usize * header.e_shentsize as usize;
    read_at(file, offset)
}

/// Locates a section by name via the section header string table
/// (`e_shstrndx`) - the section headers survive this project's own
/// `objcopy --strip-all` (confirmed by direct experiment: `--strip-all`
/// removes the symbol table and debug info, not section headers or their
/// names), so this doesn't need `.dynamic`'s `DT_RELA`/`DT_RELASZ`
/// entries at all, even though the linker script keeps `.dynamic` around
/// (LLD requires it to produce a well-formed `ET_DYN`).
fn find_section_by_name(file: &[u8], header: &ElfHeader, name: &[u8]) -> Result<Option<SectionHeader>, LoaderError> {
    if header.e_shoff == 0 || header.e_shnum == 0 {
        return Ok(None);
    }
    let shstrtab_hdr = read_section_header(file, header, header.e_shstrndx)?;
    let strtab_start = shstrtab_hdr.sh_offset as usize;
    let strtab_end = strtab_start.checked_add(shstrtab_hdr.sh_size as usize).ok_or(LoaderError::Truncated)?;
    if strtab_end > file.len() {
        return Err(LoaderError::Truncated);
    }
    let strtab = &file[strtab_start..strtab_end];

    for i in 0..header.e_shnum {
        let sh = read_section_header(file, header, i)?;
        let name_off = sh.sh_name as usize;
        if name_off >= strtab.len() {
            continue;
        }
        let name_end = strtab[name_off..].iter().position(|&b| b == 0).map_or(strtab.len(), |p| name_off + p);
        if &strtab[name_off..name_end] == name {
            return Ok(Some(sh));
        }
    }
    Ok(None)
}

/// Copies every `PT_LOAD` segment's file bytes to `region_base +
/// p_vaddr`, then zeroes `p_memsz - p_filesz` bytes past that - a real
/// `.bss` region (if a program ever has one - `programs/linker.ld` still
/// `ASSERT`s it doesn't, see that file's own doc comment) falls out of
/// this for free, it isn't a separate feature.
///
/// # Safety
/// `region_base` must point to a freshly allocated, writable region of at
/// least `region_size` bytes, and every `PT_LOAD` segment's `p_vaddr +
/// p_memsz` must fit within it (true by construction - the caller derives
/// `region_size` from exactly this).
unsafe fn copy_segments(file: &[u8], phdrs: &[ProgramHeader], region_base: u64) -> Result<(), LoaderError> {
    for ph in phdrs {
        if ph.p_type != PT_LOAD {
            continue;
        }
        let src_start = ph.p_offset as usize;
        let src_end = src_start.checked_add(ph.p_filesz as usize).ok_or(LoaderError::Truncated)?;
        if src_end > file.len() {
            return Err(LoaderError::Truncated);
        }
        let dst = (region_base + ph.p_vaddr) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(file[src_start..src_end].as_ptr(), dst, ph.p_filesz as usize);
            let bss_len = ph.p_memsz - ph.p_filesz;
            if bss_len > 0 {
                core::ptr::write_bytes(dst.add(ph.p_filesz as usize), 0, bss_len as usize);
            }
        }
    }
    Ok(())
}

/// Applies every `R_AARCH64_RELATIVE` entry in `.rela.dyn` (if the
/// program has one at all - a sufficiently trivial program legitimately
/// might not, that's not an error) against `region_base`. Per the
/// AArch64 ELF psABI, `R_AARCH64_RELATIVE`'s value is `base + addend`,
/// written at `base + offset` - both computed against wherever this
/// program actually landed, not its link-time base of `0x0`. This is the
/// actual mechanism this whole milestone is for: the exact fix for the
/// `core::fmt`/literal-comparison crash class.
///
/// # Safety
/// Same as [`copy_segments`] - `region_base` must point to the same
/// freshly-loaded, writable region `.rela.dyn`'s offsets were computed
/// against (true after `copy_segments` has already run for this program).
unsafe fn apply_relocations(file: &[u8], rela_shdr: &SectionHeader, region_base: u64) -> Result<(), LoaderError> {
    let entry_size = size_of::<Rela>();
    let table_start = rela_shdr.sh_offset as usize;
    let table_end = table_start.checked_add(rela_shdr.sh_size as usize).ok_or(LoaderError::Truncated)?;
    if table_end > file.len() {
        return Err(LoaderError::Truncated);
    }
    let count = rela_shdr.sh_size as usize / entry_size;

    for i in 0..count {
        let rela: Rela = read_at(file, table_start + i * entry_size)?;
        let r_type = (rela.r_info & 0xffff_ffff) as u32;
        if r_type != R_AARCH64_RELATIVE {
            return Err(LoaderError::UnsupportedRelocation(r_type));
        }
        let target = region_base.wrapping_add(rela.r_offset);
        let value = region_base.wrapping_add(rela.r_addend as u64);
        unsafe {
            core::ptr::write_unaligned(target as *mut u64, value);
        }
    }
    Ok(())
}

/// Parses just enough of an ELF file to know how big an EL0 region it
/// needs - no allocation, no copying. Split out from what used to be one
/// monolithic `load_elf_into_el0_region` specifically so a *runtime*
/// caller (the `spawn` syscall, `syscall.rs` - running long after
/// `exit_boot_services` has made `boot::allocate_pages` permanently
/// unavailable) can reuse this half without the boot-services-only
/// allocation the other half needs: call this first to learn the size,
/// allocate accordingly (`tasks::allocate_runtime_region`, not
/// `boot::allocate_pages`), then call [`populate_region`] with the
/// result, exactly the same two-step shape [`load_elf_into_el0_region`]
/// uses internally, just with a different allocation source in between.
pub(crate) fn elf_region_size(program: &[u8]) -> Result<(ElfHeader, ProgramHeaders, u64), LoaderError> {
    let header = parse_elf_header(program)?;
    let phdrs = parse_program_headers(program, &header)?;

    let mut max_end = 0u64;
    for ph in phdrs.as_slice() {
        if ph.p_type == PT_LOAD {
            let end = ph.p_vaddr.checked_add(ph.p_memsz).ok_or(LoaderError::Truncated)?;
            max_end = max_end.max(end);
        }
    }
    if max_end == 0 {
        return Err(LoaderError::NoLoadSegments);
    }

    let page_size = PAGE_SIZE as u64;
    let code_pages = max_end.div_ceil(page_size);
    // Layout: [code_pages][HEAP_PAGES heap][1 guard page][STACK_PAGES
    // stack pages]. The stack sits at the top (sp_el0 = base + size); the
    // guard page, immediately below it, is mapped EL1-only by mmu.rs so a
    // stack overflow faults cleanly; the heap (a raw buffer the program
    // reaches via `heap_info`) is below the guard, above the code.
    let region_pages = code_pages + HEAP_PAGES + GUARD_PAGES + STACK_PAGES;
    let region_size = region_pages * page_size;

    Ok((header, phdrs, region_size))
}

/// Copies `program`'s `PT_LOAD` segments into an *already-allocated*
/// `region_base`/`region_size` region and applies its relocations -
/// everything [`load_elf_into_el0_region`] used to do after obtaining
/// memory, factored out so a runtime caller with its own way of getting a
/// region (`tasks::allocate_runtime_region`, not `boot::allocate_pages`)
/// can reuse it unchanged.
///
/// # Safety
/// `region_base` must point to a freshly allocated, writable region of at
/// least `region_size` bytes - the same requirement [`copy_segments`] and
/// [`apply_relocations`] already documented individually.
pub(crate) unsafe fn populate_region(
    program: &[u8],
    header: &ElfHeader,
    phdrs: &[ProgramHeader],
    region_base: u64,
    region_size: u64,
) -> Result<LoadedProgram, LoaderError> {
    // SAFETY: forwarded from this function's own contract.
    unsafe { copy_segments(program, phdrs, region_base)? };

    if let Some(rela_shdr) = find_section_by_name(program, header, b".rela.dyn")? {
        // SAFETY: segments were just copied into this same region above.
        unsafe { apply_relocations(program, &rela_shdr, region_base)? };
    }

    Ok(LoadedProgram { base: region_base, size: region_size, entry: region_base + header.e_entry })
}

fn load_elf_into_el0_region(program: &[u8]) -> Result<LoadedProgram, LoaderError> {
    let (header, phdrs, region_size) = elf_region_size(program)?;
    let page_size = PAGE_SIZE as u64;

    // Over-allocate by one slot's worth of pages so there's guaranteed to
    // be a 2MB-aligned address somewhere inside the range, then trim the
    // slack off both ends - see module doc comment.
    let region_pages = region_size / page_size;
    let padded_pages = region_pages + (SLOT_ALIGN / page_size);
    let raw = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, padded_pages as usize)
        .map_err(LoaderError::Alloc)?;
    let raw_addr = raw.as_ptr() as u64;

    let aligned_addr = raw_addr.next_multiple_of(SLOT_ALIGN);
    let leading_pages = (aligned_addr - raw_addr) / page_size;
    let trailing_pages = padded_pages - leading_pages - region_pages;

    if leading_pages > 0 {
        // SAFETY: `raw` was allocated by the call above; freeing its
        // leading sub-range is valid per the UEFI spec (FreePages accepts
        // any page-aligned range within a prior allocation).
        unsafe { boot::free_pages(raw, leading_pages as usize).map_err(LoaderError::Alloc)? };
    }
    if trailing_pages > 0 {
        let trailing_addr = aligned_addr + region_size;
        // SAFETY: same as above, for the trailing sub-range.
        unsafe {
            let trailing_ptr = NonNull::new(trailing_addr as *mut u8).unwrap();
            boot::free_pages(trailing_ptr, trailing_pages as usize).map_err(LoaderError::Alloc)?;
        }
    }

    // SAFETY: `aligned_addr` is freshly allocated, page-aligned, and
    // `region_size` was derived from the same PT_LOAD segments this
    // populates.
    unsafe { populate_region(program, &header, phdrs.as_slice(), aligned_addr, region_size) }
}
