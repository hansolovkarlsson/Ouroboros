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
//! rebuild required — the actual "configuration" the current milestone is
//! about.
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

use alloc::string::String;
use core::ptr::NonNull;

use uefi::boot::{self, AllocateType, MemoryType, PAGE_SIZE};
use uefi::fs::FileSystem;
use uefi::CString16;

/// Fixed for now, not itself configurable - has to live somewhere the
/// kernel knows to look without already having read a config file to find
/// it. Everything downstream of this one path is configuration-driven.
///
/// `OUROBORO`, not `OUROBOROS` - deliberately 8 characters, not 9. Real
/// FAT32 formatters store any longer name in a long-filename (LFN) entry
/// alongside a mangled 8.3 alias (`OUROBO~2`) for compatibility, and
/// `fat32.rs`'s runtime reader doesn't parse LFN entries - so a 9-letter
/// directory name here would make this same path unreachable by the
/// shell's own `cd`/`ls` once phase 3c wires them up. This project
/// controls the name, so it's the name that gives, not a parser feature
/// added just to accommodate one avoidable 9-letter directory.
const CONFIG_PATH: &str = "\\EFI\\OUROBORO\\INIT.CFG";

const STACK_PAGES: u64 = 2; // 8KB - headroom for an unoptimized debug build's stack frames.
const SLOT_ALIGN: u64 = 0x20_0000; // 2MB - see module doc comment.

#[derive(Debug)]
pub enum LoaderError {
    Protocol(uefi::Error),
    Config(uefi::fs::Error),
    ConfigNotUtf8,
    ConfigEmpty,
    PathEncoding,
    Program(uefi::fs::Error),
    ProgramEmpty,
    Alloc(uefi::Error),
}

// A hand-written impl, not just the derived Debug above: rustc's dead-code
// analysis deliberately doesn't count a field as "used" just because a
// `#[derive(Debug)]` prints it (real code has to consume it), so without
// this every variant's wrapped error would warn as unread - even though
// printing it is exactly the point. main.rs logs through this, not Debug.
impl core::fmt::Display for LoaderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoaderError::Protocol(e) => write!(f, "couldn't open the boot volume: {e}"),
            LoaderError::Config(e) => write!(f, "couldn't read {CONFIG_PATH}: {e}"),
            LoaderError::ConfigNotUtf8 => write!(f, "{CONFIG_PATH} is not valid UTF-8"),
            LoaderError::ConfigEmpty => write!(f, "{CONFIG_PATH} is empty"),
            LoaderError::PathEncoding => write!(f, "program path isn't valid for a UEFI file path"),
            LoaderError::Program(e) => write!(f, "couldn't read the program named in {CONFIG_PATH}: {e}"),
            LoaderError::ProgramEmpty => write!(f, "program named in {CONFIG_PATH} is empty"),
            LoaderError::Alloc(e) => write!(f, "couldn't allocate memory for the program: {e}"),
        }
    }
}

/// Where a loaded program ended up and how big its whole EL0-accessible
/// region is (code, at the base, followed by whatever padding then a
/// stack growing down from the top - see `tasks.rs`'s use of this).
pub struct LoadedProgram {
    pub base: u64,
    pub size: u64,
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

    load_into_el0_region(&program_bytes)
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

fn load_into_el0_region(program: &[u8]) -> Result<LoadedProgram, LoaderError> {
    let page_size = PAGE_SIZE as u64;
    let code_pages = (program.len() as u64).div_ceil(page_size);
    let region_pages = code_pages + STACK_PAGES;
    let region_size = region_pages * page_size;

    // Over-allocate by one slot's worth of pages so there's guaranteed to
    // be a 2MB-aligned address somewhere inside the range, then trim the
    // slack off both ends - see module doc comment.
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

    // SAFETY: `aligned_addr` is freshly allocated, page-aligned, and sized
    // for at least `program.len()` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(program.as_ptr(), aligned_addr as *mut u8, program.len());
    }

    Ok(LoadedProgram { base: aligned_addr, size: region_size })
}
