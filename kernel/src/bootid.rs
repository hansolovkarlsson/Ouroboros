//! The **boot identity**: a counter raised once per boot and persisted before
//! `ExitBootServices`, and the firmware's boot entropy if it has any. Step 3 of
//! `docs/roadmap/roadmap-session-auth.md` (Decision 6): the counter makes a
//! session's ephemeral key differ across reboots, and the entropy makes it
//! unguessable where the firmware can supply some. [`syscall_abi::BOOT_ID`]
//! hands both to userland.
//!
//! ## The rules, and why each one
//!
//! - **Two stores**: a UEFI non-volatile variable (firmware storage, so rolling
//!   the disk back does not roll it back) and a file on the ESP,
//!   [`FILE_PATH`], staged at image build as `/etc/cluster/id` is.
//! - **A value counts only if it was there before this boot.** A read-back in
//!   the same boot proves nothing: edk2 as QEMU boots it here has no variable
//!   file, keeps variables in RAM, and reads a write back perfectly until the
//!   reboot that forgets it. Reading before writing catches that without a
//!   second boot, because a store that forgets never has an old value.
//! - **Next = the larger trusted value + 1, written to both.** So when the
//!   variable store starts or stops working, the next value is still above
//!   anything either store has handed out.
//! - **Usable only if EVERY store that held a value before this boot reads
//!   the new one back.** Not just one of them: a store can hold a value from
//!   before this boot and still not be durable (some edk2 builds keep their RAM
//!   variable store across a warm reset, and lose it at power-off), so a
//!   variable alone vouching for the new value while the file write failed
//!   would leave the file behind, and a later boot would hand out the same
//!   counter again (review of #163). Any store that did not take the new value
//!   means no counter this boot, and the node keys no sessions: fail-safe, not
//!   fail-open.
//! - **What the read-back proves, and what it does not.** It shows the
//!   firmware accepted the write. It does not show the write reached the
//!   medium: edk2's FAT driver caches, so the read-back of the file can come
//!   from that cache. Durability is proven only by the NEXT boot finding the
//!   value "from before this boot", which is what the two-boot check measures.
//! - **The file is rewritten in place, never deleted.** `uefi::fs`'s `write`
//!   deletes and recreates, and a reset between the two would lose the only
//!   copy. The record is fixed width ([`RECORD_LEN`]), so writing it over
//!   itself needs no truncation.
//!
//! Everything here runs before `exit_boot_services`, where `log::*` works; the
//! result is read afterwards through [`get`].

use crate::synccell::SyncCell;
use uefi::boot;
use uefi::proto::media::file::{File, FileAttribute, FileMode, RegularFile};
use uefi::proto::rng::Rng;
use uefi::runtime::{VariableAttributes, VariableVendor};
use uefi::{cstr16, guid, CStr16};

/// The counter file on the ESP: 20 decimal digits and a newline.
const FILE_PATH: &CStr16 = cstr16!("\\EFI\\ORBS\\BOOTID.TXT");
/// Bytes in the file's one record.
const RECORD_LEN: usize = 21;
/// The variable's name, under this project's own vendor GUID.
const VAR_NAME: &CStr16 = cstr16!("OuroborosBootId");
const VAR_VENDOR: VariableVendor = VariableVendor(guid!("0217fcbd-c3fa-42d4-b027-51903f6a7f6c"));
/// Bytes of boot entropy asked of `EFI_RNG_PROTOCOL`: the ABI's bound, so a
/// userland buffer sized by it always fits.
pub(crate) const ENTROPY_MAX: usize = syscall_abi::BOOT_ID_ENTROPY_MAX;

/// What this boot established, read by the `BOOT_ID` syscall.
pub(crate) struct BootIdentity {
    /// This boot's counter, or `None` when no store could vouch for one.
    pub(crate) counter: Option<u64>,
    /// `BOOT_ID_STORE_*` bits: the stores that held a value from before this boot.
    pub(crate) stores: u64,
    pub(crate) entropy: [u8; ENTROPY_MAX],
    pub(crate) entropy_len: usize,
}

static IDENTITY: SyncCell<BootIdentity> =
    SyncCell::new(BootIdentity { counter: None, stores: 0, entropy: [0; ENTROPY_MAX], entropy_len: 0 });

/// This boot's identity. Written once, by [`establish`], before anything could
/// read it.
pub(crate) fn get() -> &'static BootIdentity {
    // SAFETY: single-core; `establish` finished before exit_boot_services, and
    // nothing writes it after.
    unsafe { &*IDENTITY.get() }
}

/// Raise, persist and read back the counter, and collect the boot entropy.
/// Must run before `exit_boot_services`.
pub(crate) fn establish() {
    let var_before = read_var();
    let file_before = read_file();
    let mut stores = 0;
    if var_before.is_some() {
        stores |= syscall_abi::BOOT_ID_STORE_VARIABLE;
    }
    if file_before.is_some() {
        stores |= syscall_abi::BOOT_ID_STORE_FILE;
    }

    let counter = match var_before.max(file_before) {
        None => {
            log::warn!("Ouroboros kernel: boot identity: no store held a counter from before this boot - no counter, sessions will not be keyed");
            None
        }
        // Never raised onto the syscall's "none" value.
        Some(v) if v >= syscall_abi::BOOT_ID_NONE - 1 => {
            log::warn!("Ouroboros kernel: boot identity: counter {v} is at its limit - no counter");
            None
        }
        Some(v) => {
            let next = v + 1;
            write_var(next);
            write_file(next);
            // Every store that held a value from before this boot must now
            // hold the new one; a store that held none is written but not
            // required (it has not yet shown it survives a reboot).
            let var_ok = var_before.is_none() || read_var() == Some(next);
            let file_ok = file_before.is_none() || read_file() == Some(next);
            if var_ok && file_ok {
                Some(next)
            } else {
                log::warn!(
                    "Ouroboros kernel: boot identity: counter {next} did not read back from every store that held one before (variable {}, file {}) - no counter",
                    if var_ok { "ok" } else { "FAILED" },
                    if file_ok { "ok" } else { "FAILED" }
                );
                None
            }
        }
    };

    let mut entropy = [0u8; ENTROPY_MAX];
    let entropy_len = boot_entropy(&mut entropy);

    match counter {
        Some(c) => log::info!(
            "Ouroboros kernel: boot identity: boot {c}; from before this boot: variable {}, file {}; boot entropy {entropy_len} bytes",
            describe(var_before),
            describe(file_before)
        ),
        None => log::info!(
            "Ouroboros kernel: boot identity: NO COUNTER; from before this boot: variable {}, file {}; boot entropy {entropy_len} bytes",
            describe(var_before),
            describe(file_before)
        ),
    }

    // SAFETY: single-core, before exit_boot_services; no reader exists yet.
    unsafe {
        *IDENTITY.get() = BootIdentity { counter, stores, entropy, entropy_len };
    }
}

fn describe(v: Option<u64>) -> &'static str {
    if v.is_some() {
        "present"
    } else {
        "absent"
    }
}

/// The variable's value, if it exists and is exactly eight bytes.
fn read_var() -> Option<u64> {
    let mut buf = [0u8; 8];
    match uefi::runtime::get_variable(VAR_NAME, &VAR_VENDOR, &mut buf) {
        Ok((data, _)) if data.len() == 8 => Some(u64::from_le_bytes(buf)),
        _ => None,
    }
}

fn write_var(v: u64) {
    let attrs = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS;
    if let Err(e) = uefi::runtime::set_variable(VAR_NAME, &VAR_VENDOR, attrs, &v.to_le_bytes()) {
        log::warn!("Ouroboros kernel: boot identity: variable write failed: {:?}", e.status());
    }
}

/// The counter file, opened from the ESP's root. Opened afresh for each use,
/// which is simple rather than protective: a read after a write can still be
/// served from the firmware's FAT cache (see the module doc).
fn open_file(mode: FileMode) -> Option<RegularFile> {
    let mut sfs = boot::get_image_file_system(boot::image_handle()).ok()?;
    let mut root = sfs.open_volume().ok()?;
    root.open(FILE_PATH, mode, FileAttribute::empty()).ok()?.into_regular_file()
}

/// The file's counter, if the file is exactly one well-formed record. Anything
/// else is not trusted: a half-written or hand-edited file must not become a
/// counter.
fn read_file() -> Option<u64> {
    let mut f = open_file(FileMode::Read)?;
    let mut buf = [0u8; RECORD_LEN + 1];
    let n = f.read(&mut buf).ok()?;
    if n != RECORD_LEN || buf[RECORD_LEN - 1] != b'\n' {
        return None;
    }
    let mut v: u64 = 0;
    for &d in &buf[..RECORD_LEN - 1] {
        if !d.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((d - b'0') as u64)?;
    }
    Some(v)
}

/// Write `v` as the file's one record, in place. A file of any length other than
/// one record (or zero, a file this write creates) is left alone, because
/// writing 21 bytes over a longer one would leave a tail. A 21-byte file is
/// written over whatever it holds: if it was not a valid record it was never
/// trusted, and this repairs it.
fn write_file(v: u64) {
    let mut rec = [b'0'; RECORD_LEN];
    rec[RECORD_LEN - 1] = b'\n';
    let mut x = v;
    for i in (0..RECORD_LEN - 1).rev() {
        rec[i] = b'0' + (x % 10) as u8;
        x /= 10;
    }
    let Some(mut f) = open_file(FileMode::CreateReadWrite) else {
        log::warn!("Ouroboros kernel: boot identity: cannot open the counter file for writing");
        return;
    };
    // Only a record, or a file this write creates (length 0), is written over.
    let mut probe = [0u8; RECORD_LEN + 1];
    let len = f.read(&mut probe).unwrap_or(usize::MAX);
    if len != 0 && len != RECORD_LEN {
        log::warn!("Ouroboros kernel: boot identity: the counter file is not one record; left as it is");
        return;
    }
    if f.set_position(0).is_err() || f.write(&rec).is_err() || f.flush().is_err() {
        log::warn!("Ouroboros kernel: boot identity: counter file write failed");
    }
}

/// Up to [`ENTROPY_MAX`] bytes from `EFI_RNG_PROTOCOL`, or 0 where the firmware
/// offers none (on Parallels and the Pi, to be measured). Opened with
/// `GetProtocol`, not exclusively: an exclusive open fails, or disconnects the
/// holder, where firmware already has the protocol open, and this would then
/// report "no entropy" on a platform that has it (review of #163). A protocol
/// that is present but fails is logged apart from one that is absent, since
/// which of the two a platform is is what step 3 measures.
fn boot_entropy(out: &mut [u8; ENTROPY_MAX]) -> usize {
    let Ok(handle) = boot::get_handle_for_protocol::<Rng>() else {
        return 0;
    };
    let params = boot::OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: GetProtocol gives no exclusivity; the protocol is used only
    // inside this function, before exit_boot_services, and nothing here
    // uninstalls it.
    let opened = unsafe { boot::open_protocol::<Rng>(params, boot::OpenProtocolAttributes::GetProtocol) };
    let mut rng = match opened {
        Ok(rng) => rng,
        Err(e) => {
            log::warn!("Ouroboros kernel: boot identity: EFI_RNG_PROTOCOL present but would not open: {:?}", e.status());
            return 0;
        }
    };
    match rng.get_rng(None, out) {
        Ok(()) => ENTROPY_MAX,
        Err(e) => {
            *out = [0; ENTROPY_MAX];
            log::warn!("Ouroboros kernel: boot identity: EFI_RNG_PROTOCOL present but failed: {:?}", e.status());
            0
        }
    }
}
