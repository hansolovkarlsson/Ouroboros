//! USB Mass Storage over Bulk-Only Transport (BOT) with the
//! SCSI-transparent command set - the disk driver real Parallels
//! hardware actually needed: it exposes no storage controller of any
//! kind on the PCI bus (confirmed by direct diagnosis - see CLAUDE.md's
//! "Parallels disk diagnostic"), but a passed-through USB 3.x stick
//! lands on the xHCI controller this kernel already drives and presents
//! exactly interface `class=0x08 subclass=0x06 protocol=0x50`
//! (confirmed by the enumeration checks, on QEMU's `usb-storage` and
//! the real stick alike).
//!
//! Modeled on `virtio_blk.rs`'s synchronous, polling, one-command-at-a-
//! time shape. The transport is `xhci.rs`'s bulk endpoint pair
//! (`storage_bulk`), configured by the multi-device scan's
//! `activate_storage`. One SCSI command = three bulk transfers:
//!
//! 1. **CBW** (Command Block Wrapper, 31 bytes) on bulk OUT - the
//!    `USBC` signature, a tag echoed back in the CSW, the expected data
//!    length and direction, LUN 0, and the SCSI CDB itself.
//! 2. The **data stage** (if any) on the matching direction's ring.
//! 3. **CSW** (Command Status Wrapper, 13 bytes) on bulk IN - `USBS`
//!    signature, the echoed tag, a residue count, and the status byte
//!    (0 = good).
//!
//! Commands implemented: `INQUIRY` (logged vendor/product strings - the
//! "it's really talking to the stick" proof), `READ CAPACITY(10)`
//! (last LBA + block size, both big-endian; block sizes other than 512
//! are refused outright - `fat32.rs` assumes 512 throughout), and
//! `READ(10)`/`WRITE(10)` one sector per call (`fat32.rs` is
//! sector-at-a-time anyway; throughput is explicitly a non-goal).
//!
//! **No BOT error recovery** - a failed or stalled command is reported
//! and the caller's operation fails; the spec's Reset Recovery sequence
//! (Bulk-Only Mass Storage Reset + clear both endpoint stalls) is a
//! known, documented gap, same posture as the keyboard interrupt
//! endpoint's stall handling.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::console;
use crate::xhci;

const CBW_SIGNATURE: u32 = 0x4342_5355; // "USBC" little-endian
const CSW_SIGNATURE: u32 = 0x5342_5355; // "USBS" little-endian
const CBW_FLAGS_DATA_IN: u8 = 0x80;

#[derive(Debug)]
pub enum Error {
    /// No activated storage device (nothing enumerated, or activation
    /// failed).
    NoDevice,
    /// A bulk transfer failed at the xHCI level (timeout, stall, ...).
    Transfer(xhci::Error),
    /// The CSW came back with a bad signature or a tag that doesn't
    /// match the command it should answer.
    CswMismatch,
    /// The device reported the command failed (CSW status != 0).
    CommandFailed(u8),
    /// The device's block size isn't the 512 bytes `fat32.rs` assumes.
    UnsupportedBlockSize(u32),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoDevice => write!(f, "no USB storage device"),
            Error::Transfer(e) => write!(f, "bulk transfer failed ({e})"),
            Error::CswMismatch => write!(f, "CSW signature/tag mismatch"),
            Error::CommandFailed(status) => write!(f, "command failed (CSW status {status})"),
            Error::UnsupportedBlockSize(n) => write!(f, "unsupported block size {n} (only 512 is implemented)"),
        }
    }
}

#[repr(align(64))]
struct Aligned64<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Aligned64<T> {}

// DMA buffers - single-instance statics, same idiom as virtio_blk.rs's
// rings and xhci.rs's CTRL_BUF/INT_BUF: exactly one command in flight
// ever (every caller is the single-core SVC/boot path).
static CBW_BUF: Aligned64<[u8; 31]> = Aligned64(UnsafeCell::new([0; 31]));
static CSW_BUF: Aligned64<[u8; 13]> = Aligned64(UnsafeCell::new([0; 13]));
static DATA_BUF: Aligned64<[u8; 512]> = Aligned64(UnsafeCell::new([0; 512]));

/// Monotonic CBW tag - echoed back in each CSW and checked, so a stale
/// or misrouted status can't be mistaken for the current command's.
static NEXT_TAG: AtomicU32 = AtomicU32::new(1);

/// A mounted-capacity handle - deliberately tiny: the endpoint state
/// lives in `xhci.rs`, the DMA buffers are statics, so all a device
/// *is* here is its validated capacity.
pub struct Device {
    /// Validated at init (logged there too) - reported to the
    /// filesystem server via the `BLOCK_INFO` syscall.
    capacity_sectors: u64,
}

impl Device {
    /// Probes the activated storage device: `INQUIRY` (logged) and
    /// `READ CAPACITY(10)` (validated to 512-byte blocks). The
    /// "driver really works" moment for a new device.
    pub fn init() -> Result<Self, Error> {
        if !xhci::storage_present() {
            return Err(Error::NoDevice);
        }

        // INQUIRY: 36 bytes of standard inquiry data - bytes 8..16 are
        // the T10 vendor string, 16..32 the product string, both
        // space-padded ASCII.
        let mut inquiry = [0u8; 36];
        bot_command(&[0x12, 0, 0, 0, 36, 0], Some((&mut inquiry, true)))?;
        let vendor = core::str::from_utf8(&inquiry[8..16]).unwrap_or("?").trim_ascii_end();
        let product = core::str::from_utf8(&inquiry[16..32]).unwrap_or("?").trim_ascii_end();
        console::println!("Ouroboros kernel: usb-msd: INQUIRY -> vendor='{vendor}' product='{product}'");

        // A freshly attached/reset device reports a Unit Attention
        // condition: the first non-INQUIRY command fails with CHECK
        // CONDITION (CSW status 1) until the sense data is fetched -
        // found organically by the hot-plug rescan test (INQUIRY
        // succeeded, READ CAPACITY failed, a retry succeeded; INQUIRY
        // is spec-exempt from Unit Attention, which fit exactly).
        // Standard bring-up clears it: TEST UNIT READY, and on failure
        // REQUEST SENSE to consume the pending sense data, a few times.
        for _ in 0..3 {
            if bot_command(&[0x00, 0, 0, 0, 0, 0], None).is_ok() {
                break; // TEST UNIT READY passed - no pending condition
            }
            let mut sense = [0u8; 18];
            let _ = bot_command(&[0x03, 0, 0, 0, 18, 0], Some((&mut sense, true)));
        }

        // READ CAPACITY(10): 8 bytes - big-endian last LBA, big-endian
        // block size.
        let mut cap = [0u8; 8];
        bot_command(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], Some((&mut cap, true)))?;
        let last_lba = u32::from_be_bytes([cap[0], cap[1], cap[2], cap[3]]);
        let block_size = u32::from_be_bytes([cap[4], cap[5], cap[6], cap[7]]);
        if block_size != 512 {
            return Err(Error::UnsupportedBlockSize(block_size));
        }
        let capacity_sectors = last_lba as u64 + 1;
        console::println!("Ouroboros kernel: usb-msd: capacity {capacity_sectors} sectors ({block_size}-byte blocks)");

        Ok(Device { capacity_sectors })
    }

    /// The device's capacity in 512-byte sectors, as validated by
    /// `READ CAPACITY(10)` at init.
    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// Reads one 512-byte sector via `READ(10)`.
    ///
    /// # Safety
    /// Mirrors `virtio_blk::Device::read_sector`'s contract shape (a
    /// successfully-constructed `Device` implies the transport is live);
    /// kept `unsafe` for signature symmetry with the other block driver.
    pub unsafe fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> Result<(), Error> {
        let lba = sector as u32;
        let cdb = [
            0x28, // READ(10)
            0,
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            0, // transfer length (big-endian, sectors) ...
            1, // ... = 1
            0,
        ];
        bot_command(&cdb, Some((buf, true)))?;
        Ok(())
    }

    /// Writes one 512-byte sector via `WRITE(10)` - same shape as
    /// [`Self::read_sector`] with the data stage reversed.
    ///
    /// # Safety
    /// Same contract as [`Self::read_sector`].
    pub unsafe fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> Result<(), Error> {
        let lba = sector as u32;
        let cdb = [
            0x2a, // WRITE(10)
            0,
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            0,
            1, // one sector
            0,
        ];
        // bot_command's data parameter is `&mut` for the IN case's
        // copy-back; a local copy keeps write_sector's own signature
        // honest (`&[u8; 512]`, matching virtio_blk's).
        let mut data = *buf;
        bot_command(&cdb, Some((&mut data, false)))?;
        Ok(())
    }
}

/// One full BOT command: CBW out, optional data stage (`Some((buffer,
/// data_in))`), CSW in, status checked. The data stage goes through the
/// static `DATA_BUF` (bounce buffer) so callers can pass ordinary stack
/// slices without DMA-lifetime concerns.
fn bot_command(cdb: &[u8], data: Option<(&mut [u8], bool)>) -> Result<(), Error> {
    let tag = NEXT_TAG.fetch_add(1, Ordering::Relaxed);
    let (data_len, data_in) = match &data {
        Some((buf, dir_in)) => (buf.len() as u32, *dir_in),
        None => (0, true),
    };

    // Build the CBW.
    {
        let cbw = unsafe { &mut *CBW_BUF.0.get() };
        cbw.fill(0);
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&data_len.to_le_bytes());
        cbw[12] = if data_in { CBW_FLAGS_DATA_IN } else { 0 };
        cbw[13] = 0; // LUN 0
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);
    }
    xhci::storage_bulk(false, CBW_BUF.0.get() as u64, 31).map_err(Error::Transfer)?;

    // Data stage, bounced through DATA_BUF.
    if let Some((buf, dir_in)) = data {
        let len = buf.len().min(512);
        if dir_in {
            xhci::storage_bulk(true, DATA_BUF.0.get() as u64, len as u32).map_err(Error::Transfer)?;
            let src = DATA_BUF.0.get().cast::<u8>();
            for (i, b) in buf[..len].iter_mut().enumerate() {
                *b = unsafe { read_volatile(src.add(i)) };
            }
        } else {
            let dst = DATA_BUF.0.get().cast::<u8>();
            for (i, b) in buf[..len].iter().enumerate() {
                unsafe { write_volatile(dst.add(i), *b) };
            }
            xhci::storage_bulk(false, DATA_BUF.0.get() as u64, len as u32).map_err(Error::Transfer)?;
        }
    }

    // CSW.
    xhci::storage_bulk(true, CSW_BUF.0.get() as u64, 13).map_err(Error::Transfer)?;
    let csw = unsafe { &*CSW_BUF.0.get() };
    let signature = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
    let echoed_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
    if signature != CSW_SIGNATURE || echoed_tag != tag {
        return Err(Error::CswMismatch);
    }
    if csw[12] != 0 {
        return Err(Error::CommandFailed(csw[12]));
    }
    Ok(())
}
