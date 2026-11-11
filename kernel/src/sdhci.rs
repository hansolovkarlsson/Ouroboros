//! An SD card on a standard SD Host Controller (SDHCI 2.00 or 3.00), read by
//! PIO: the SD-card arc's driver (`docs/roadmap/roadmap-sd-card.md`), the
//! third `block.rs` arm beside `virtio_blk` and `usb_msd`. It reads (step
//! 1) and writes (step 2), one sector at a time.
//!
//! **The firmware's partition is never written** (decision D3): the Pi's
//! firmware keeps its variable store, `RPI_EFI.fd`, as a file in it. Nor
//! is the partition table that finds it: sector 0 to the partition's end,
//! and a GPT's backup copy where its headers put it. The sectors are fixed when the
//! card is brought up (`Device::protect`, from `parttable::firmware_sectors`),
//! and a write inside them is refused here, before the controller is
//! touched, whatever asked for it; since the table is among them, nothing
//! the kernel does can move the refusal off them on a later boot either.
//!
//! **Found, never assumed.** On QEMU the controller is `sdhci-pci`, found by
//! its PCI class (`pci::discover_sdhci`, class 0x08, subclass 0x05). On the
//! Raspberry Pi 4 it is EMMC2 at the address its ACPI methods give
//! (`amlscan::declared`, `main.rs`'s `emmc2_from_acpi`, since step 3), and
//! the Pi boots and runs from the card alone since step 4.
//!
//! **The card is brought up from scratch**, by the SD Physical Layer
//! Simplified Specification, rather than from whatever the firmware left:
//! a full controller reset, 3.3 V, the identification clock (400 kHz at
//! most and never under 200 kHz: the BCM2711's EMMC2 hangs probing at
//! 100 kHz, Linux's `sdhci-iproc` says), then CMD0, CMD8, ACMD41 until the
//! card is ready, CMD2, CMD3 for its address (the RCA), CMD9 for its size
//! (the CSD), CMD7 to select it, ACMD6 for a 4-bit bus, CMD16 for 512-byte
//! blocks on a standard-capacity card, and 25 MHz. So QEMU and the Pi run
//! the same sequence from the same state.
//!
//! **Every register access is 32 bits wide and aligned.** The BCM2711's
//! SDHCI wants that, and QEMU's takes it; a byte or half-word field is
//! written by reading its word, changing the field and writing the word
//! back. Transfer mode and command share one word, written together, which
//! is also what starts the command.
//!
//! **Addressing follows the card.** A standard-capacity card (SDSC, at most
//! 2 GB) is addressed in bytes, a high-capacity one (SDHC, SDXC) in
//! 512-byte blocks, as ACMD41's CCS bit says; QEMU's `sd-card` makes a card
//! over 2 GB high-capacity, so `make test-sd` boots one of each. Every read
//! is one sector (CMD17) and so is every write (CMD24): `BLOCK_READ` and
//! `BLOCK_WRITE` are one sector, so a multi-block command would be code no
//! caller reaches.
//!
//! Polled throughout: no interrupt is signalled (the status-enable bits are
//! set so the status register latches, the signal-enable bits are clear),
//! and each wait is bounded by elapsed time (`timer::now_ticks`), as
//! `xhci.rs`'s are.

use crate::console;
use crate::timer;
use core::ptr::{read_volatile, write_volatile};

// The registers, as 32-bit words (SDHCI 4.20, section 2.1).
const BLOCK_SIZE_COUNT: usize = 0x04;
const ARGUMENT: usize = 0x08;
const TRANSFER_MODE_COMMAND: usize = 0x0c;
const RESPONSE: usize = 0x10;
const BUFFER_DATA: usize = 0x20;
const PRESENT_STATE: usize = 0x24;
const HOST_CONTROL: usize = 0x28;
const CLOCK_CONTROL: usize = 0x2c;
const INT_STATUS: usize = 0x30;
const INT_STATUS_ENABLE: usize = 0x34;
const INT_SIGNAL_ENABLE: usize = 0x38;
const CAPABILITIES: usize = 0x40;
const HOST_VERSION: usize = 0xfc;

// PRESENT_STATE bits.
const CMD_INHIBIT: u32 = 1 << 0;
const DAT_INHIBIT: u32 = 1 << 1;
const CARD_INSERTED: u32 = 1 << 16;
const CARD_STABLE: u32 = 1 << 17;
/// The write-protect switch's level: 1 is writable (SDHCI 2.2.9). A card
/// does not enforce its own lock tab; the host does.
const WRITE_ENABLED: u32 = 1 << 19;

// INT_STATUS bits: the normal half, then the error half.
const INT_COMMAND_COMPLETE: u32 = 1 << 0;
const INT_TRANSFER_COMPLETE: u32 = 1 << 1;
const INT_BUFFER_WRITE_READY: u32 = 1 << 4;
const INT_BUFFER_READ_READY: u32 = 1 << 5;
const INT_ERROR: u32 = 1 << 15;
const INT_ERROR_BITS: u32 = 0xffff_0000;

// CLOCK_CONTROL's word: the clock control half-word, the timeout control
// byte, the software reset byte.
const CLOCK_INTERNAL_ENABLE: u32 = 1 << 0;
const CLOCK_INTERNAL_STABLE: u32 = 1 << 1;
const CLOCK_SD_ENABLE: u32 = 1 << 2;
const TIMEOUT_MAX: u32 = 0x0e << 16;
const RESET_ALL: u32 = 1 << 24;
const RESET_CMD: u32 = 1 << 25;
const RESET_DAT: u32 = 1 << 26;

// HOST_CONTROL's word: host control 1, power control, block gap, wakeup.
const HOST_DATA_4BIT: u32 = 1 << 1;
/// SD Bus Voltage Select, 3.3 V (power control bits 3:1), written alone
/// before SD Bus Power (bit 0), as SDHCI orders them.
const POWER_SELECT_3V3: u32 = 0x0e << 8;
const POWER_ON: u32 = 0x01 << 8;
/// Capabilities: the controller supports 3.3 V.
const CAPS_3V3: u32 = 1 << 24;

// The command register's half-word (the word's upper half).
const RESP_NONE: u32 = 0x00;
const RESP_136: u32 = 0x01;
const RESP_48: u32 = 0x02;
const RESP_48_BUSY: u32 = 0x03;
const CMD_CRC_CHECK: u32 = 1 << 3;
const CMD_INDEX_CHECK: u32 = 1 << 4;
const CMD_DATA_PRESENT: u32 = 1 << 5;

// The transfer mode half-word (the word's lower half).
const XFER_BLOCK_COUNT_ENABLE: u32 = 1 << 1;
const XFER_READ: u32 = 1 << 4;

/// The identification clock: at most this, by the SD spec.
const IDENT_HZ: u32 = 400_000;
/// Never slower: the BCM2711's EMMC2 hangs probing at 100 kHz.
const MIN_HZ: u32 = 200_000;
/// The data clock: default speed.
const DATA_HZ: u32 = 25_000_000;

/// How long any one wait may take before it is an error.
const WAIT_MS: u64 = 1000;

const SECTOR: usize = 512;

#[derive(Debug, Clone, Copy)]
pub enum Error {
    /// The controller's capabilities name no base clock (Arasan's read 0).
    NoBaseClock,
    /// The controller does not offer 3.3 V, the one voltage used here.
    No3V3,
    /// The card-detect line says no card is in the slot.
    NoCard,
    /// The base clock cannot be divided into the identification range.
    ClockRange { base_hz: u32 },
    /// A wait ran out: what was being waited for.
    Timeout(&'static str),
    /// The controller reported an error for a command: its index and the
    /// error half of the interrupt status.
    Command { index: u8, errors: u16 },
    /// CMD8's echo did not come back as sent.
    BadEcho(u32),
    /// ACMD41 never reported the card ready.
    NotReady,
    /// The CSD's structure version is one this driver does not read.
    UnknownCsd(u8),
    /// A sector past the card's end.
    OutOfRange(u64),
    /// A write inside the firmware's partition, which is never written.
    Protected(u64),
    /// The card's write-protect switch is set.
    WriteProtected,
    /// The card's status in a response reports an error: the command and
    /// the status word's error bits.
    CardStatus { index: u8, status: u32 },
}

impl Error {
    /// Whether this is the driver declining to write, by its own rule or
    /// the card's switch, and not a write that was tried and failed.
    /// `BLOCK_WRITE` answers the two apart (`BLOCK_ERR_PROTECTED`, since
    /// 2026-11-11): until then both were an I/O error, and a program
    /// writing to the firmware partition was told the device had failed.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Error::Protected(_) | Error::WriteProtected)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoBaseClock => write!(f, "the controller names no base clock"),
            Error::No3V3 => write!(f, "the controller does not offer 3.3 V"),
            Error::NoCard => write!(f, "no card in the slot"),
            Error::ClockRange { base_hz } => write!(f, "a {base_hz} Hz base clock divides into no identification clock"),
            Error::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Error::Command { index, errors } => write!(f, "CMD{index} failed, error status {errors:#06x}"),
            Error::BadEcho(r) => write!(f, "CMD8's echo came back as {r:#x}"),
            Error::NotReady => write!(f, "the card never reported ready (ACMD41)"),
            Error::UnknownCsd(v) => write!(f, "a CSD of structure version {v}"),
            Error::OutOfRange(s) => write!(f, "sector {s} is past the card's end"),
            Error::Protected(s) => write!(f, "sector {s} is in the card's firmware partition, which is never written"),
            Error::WriteProtected => write!(f, "the card's write-protect switch is set"),
            Error::CardStatus { index, status } => write!(f, "CMD{index}: the card reports status errors {status:#010x}"),
        }
    }
}

/// One SDHCI controller with its card, once [`Device::init`] has brought
/// the card up.
pub struct Device {
    base: usize,
    rca: u32,
    high_capacity: bool,
    sectors: u64,
    /// The sectors never written, `(first, count)` each, empty ones
    /// `(0, 0)`: the firmware partition and the tables that find it.
    protected: [(u64, u64); 2],
    /// Writes refused inside it this boot; the first one is said.
    refused: u64,
    /// Whether SDCLK runs at the identification clock: the BCM2711 then
    /// wants time between register writes (`write`).
    slow_clock: bool,
}

impl Device {
    /// A controller at `base`, not yet touched.
    ///
    /// # Safety
    /// `base` is a discovered SDHCI register block, at least 0x100 bytes,
    /// mapped as device memory under this kernel's tables.
    pub unsafe fn new(base: u64) -> Self {
        Device { base: base as usize, rca: 0, high_capacity: false, sectors: 0, protected: [(0, 0); 2], refused: 0, slow_clock: false }
    }

    /// The card's size in 512-byte sectors, once initialized.
    pub fn capacity_sectors(&self) -> u64 {
        self.sectors
    }

    /// Fixes the sectors no write may reach, `(first, count)` ranges, for
    /// the rest of the boot: the firmware partition and the tables that find
    /// it, as the card's table and the boot partition gave them at bring-up
    /// (`main.rs`'s `init_sd`, `parttable::firmware_sectors`). Set once,
    /// before the card is installed.
    pub fn protect(&mut self, ranges: [(u64, u64); 2]) {
        self.protected = ranges;
    }

    /// The protected range that holds any of the `count` sectors from
    /// `first`, if one does: the one rule `write_sector` refuses by and
    /// `BLOCK_PROTECTED` answers by, so what fsd is told at a mount is
    /// what a write there would meet.
    pub fn protecting(&self, first: u64, count: u64) -> Option<(u64, u64)> {
        self.protected.into_iter().find(|&(held, len)| {
            // Two ranges meet when each starts before the other ends; an
            // empty one, `(0, 0)` or a count of 0, meets nothing.
            len > 0 && count > 0 && first < held.saturating_add(len) && held < first.saturating_add(count)
        })
    }

    /// Whether a write to any of the `count` sectors from `first` would be
    /// declined now: a protected range holds one, or the card's
    /// write-protect switch is set, read as `write_one` reads it. What
    /// `BLOCK_PROTECTED` answers, so a switched card's mount is read-only
    /// from its first line too (the review of #445; the first cut asked the
    /// ranges alone). The switch is the one answer that can change after it
    /// is given, and `write_one` still reads it at every write.
    pub fn declines(&self, first: u64, count: u64) -> bool {
        self.protecting(first, count).is_some() || self.read(PRESENT_STATE) & WRITE_ENABLED == 0
    }

    /// The command argument for `sector`: bytes on a standard-capacity
    /// card, blocks on a high-capacity one.
    fn address(&self, sector: u64) -> Result<u32, Error> {
        if sector >= self.sectors {
            return Err(Error::OutOfRange(sector));
        }
        let address = if self.high_capacity { sector } else { sector * SECTOR as u64 };
        u32::try_from(address).map_err(|_| Error::OutOfRange(sector))
    }

    fn read(&self, off: usize) -> u32 {
        // SAFETY: inside the register block `new`'s caller vouched for.
        unsafe { read_volatile((self.base + off) as *const u32) }
    }

    fn write(&self, off: usize, value: u32) {
        // SAFETY: as `read`.
        unsafe { write_volatile((self.base + off) as *mut u32, value) }
        // While SDCLK runs at the identification clock, the BCM2711's
        // controller drops a write that follows another within a few SD
        // clocks; Linux's sdhci-iproc waits after each write there. 20 us is
        // four clocks at the slowest clock used, 200 kHz. Before a clock is
        // set at all (after a reset) the same: `slow_clock` starts true at
        // bring-up.
        if self.slow_clock {
            timer::delay_us(20);
        }
    }

    /// Waits until `done` holds of the register at `off`, for at most
    /// [`WAIT_MS`]; the last value read either way.
    fn wait(&self, off: usize, what: &'static str, done: impl Fn(u32) -> bool) -> Result<u32, Error> {
        let deadline = timer::deadline_us(WAIT_MS * 1000);
        loop {
            let v = self.read(off);
            if done(v) {
                return Ok(v);
            }
            if timer::now_ticks() > deadline {
                return Err(Error::Timeout(what));
            }
            core::hint::spin_loop();
        }
    }

    /// Resets the lines in `which` (`RESET_ALL`, `RESET_CMD`, `RESET_DAT`)
    /// and waits for the reset to finish.
    fn reset(&self, which: u32) -> Result<(), Error> {
        let word = self.read(CLOCK_CONTROL) & 0x00ff_ffff;
        self.write(CLOCK_CONTROL, word | which);
        self.wait(CLOCK_CONTROL, "a reset to finish", |v| v & which == 0).map(|_| ())
    }

    /// The SDCLK divider field (clock control bits 15:6) for at most `hz`
    /// from `base_hz`, and the clock it gives. SDHCI 3.00's 10-bit divided
    /// clock is `base / 2N` (N 0 is `base` itself); 2.00's 8-bit divisor is
    /// a power of two, the same formula with N one-hot.
    fn divider(version: u32, base_hz: u32, hz: u32) -> (u32, u32) {
        if base_hz <= hz {
            return (0, base_hz);
        }
        if version >= 2 {
            let n = base_hz.div_ceil(2 * hz).min(0x3ff);
            (((n & 0xff) << 8) | ((n >> 8) << 6), base_hz / (2 * n))
        } else {
            let mut n = 1;
            while n < 0x80 && base_hz / (2 * n) > hz {
                n *= 2;
            }
            (n << 8, base_hz / (2 * n))
        }
    }

    /// Runs SDCLK at the clock `divider` gives: the SD clock off, the
    /// divider set, the internal clock on and stable, the SD clock on.
    fn set_clock(&self, field: u32) -> Result<(), Error> {
        let word = self.read(CLOCK_CONTROL) & 0xff_0000;
        self.write(CLOCK_CONTROL, word);
        self.write(CLOCK_CONTROL, word | field | CLOCK_INTERNAL_ENABLE);
        self.wait(CLOCK_CONTROL, "the internal clock to be stable", |v| v & CLOCK_INTERNAL_STABLE != 0)?;
        self.write(CLOCK_CONTROL, word | field | CLOCK_INTERNAL_ENABLE | CLOCK_SD_ENABLE);
        Ok(())
    }

    /// Sends command `index` with `arg`, `flags` naming its response and
    /// checks, `transfer` its transfer mode; waits for it to complete and
    /// returns the response registers. A command the controller fails
    /// resets the command and data lines before its error is returned.
    fn command(&mut self, index: u8, arg: u32, flags: u32, transfer: u32) -> Result<[u32; 4], Error> {
        let inhibit = if flags & CMD_DATA_PRESENT != 0 || flags & 0x3 == RESP_48_BUSY {
            CMD_INHIBIT | DAT_INHIBIT
        } else {
            CMD_INHIBIT
        };
        self.wait(PRESENT_STATE, "the command line to be free", |v| v & inhibit == 0)?;
        self.write(INT_STATUS, 0xffff_ffff);
        self.write(ARGUMENT, arg);
        let command = (u32::from(index) << 8) | flags;
        self.write(TRANSFER_MODE_COMMAND, (command << 16) | transfer);
        let status = self.wait(INT_STATUS, "a command to complete", |v| v & (INT_COMMAND_COMPLETE | INT_ERROR) != 0)?;
        if status & INT_ERROR != 0 {
            return Err(self.failed(index, status));
        }
        self.write(INT_STATUS, INT_COMMAND_COMPLETE);
        let response = [self.read(RESPONSE), self.read(RESPONSE + 4), self.read(RESPONSE + 8), self.read(RESPONSE + 12)];
        if flags & 0x3 == RESP_48_BUSY {
            self.wait_transfer(index)?;
        }
        Ok(response)
    }

    /// The error a failed command `index` leaves, its lines reset and its
    /// status cleared, so the next command starts clean.
    fn failed(&self, index: u8, status: u32) -> Error {
        let errors = ((status & INT_ERROR_BITS) >> 16) as u16;
        self.abandon();
        Error::Command { index, errors }
    }

    fn wait_transfer(&self, index: u8) -> Result<(), Error> {
        let status =
            self.wait(INT_STATUS, "a transfer to complete", |v| v & (INT_TRANSFER_COMPLETE | INT_ERROR) != 0)?;
        if status & INT_ERROR != 0 {
            return Err(self.failed(index, status));
        }
        self.write(INT_STATUS, INT_TRANSFER_COMPLETE);
        Ok(())
    }

    /// The card's status word in an R1 response, checked for the bits that
    /// report an error (SD spec 4.10.1: out of range, address, block length,
    /// erase, write-protect violation, card locked, lock or unlock failed,
    /// CRC of the previous command, illegal command, card ECC, controller,
    /// general error, CSD overwrite, write-protect erase skip, AKE sequence).
    /// Used only once the card is identified (CMD7 on): an illegal-command
    /// bit then describes this driver's own previous command, where during
    /// identification it can be the CMD8 an older card does not know.
    fn card_status(index: u8, response: [u32; 4]) -> Result<(), Error> {
        const R1_ERRORS: u32 = 0xfff9_8008;
        match response[0] & R1_ERRORS {
            0 => Ok(()),
            status => Err(Error::CardStatus { index, status }),
        }
    }

    /// An application command: CMD55 with the card's address, then `index`.
    fn app_command(&mut self, index: u8, arg: u32, flags: u32) -> Result<[u32; 4], Error> {
        self.command(55, self.rca << 16, RESP_48 | CMD_CRC_CHECK | CMD_INDEX_CHECK, 0)?;
        self.command(index, arg, flags, 0)
    }

    /// Resets the controller and brings the card up, by the module doc's
    /// sequence, logging each fact a later failure would be read against.
    ///
    /// # Safety
    /// As [`Device::new`]; nothing else drives this controller.
    pub unsafe fn init(&mut self) -> Result<(), Error> {
        const R1: u32 = RESP_48 | CMD_CRC_CHECK | CMD_INDEX_CHECK;
        let version = (self.read(HOST_VERSION) >> 16) & 0xff;
        let caps = self.read(CAPABILITIES);
        // Base clock: bits 15:8 from 3.00, 13:8 before (SDHCI 2.2.25).
        let base_mhz = if version >= 2 { (caps >> 8) & 0xff } else { (caps >> 8) & 0x3f };
        if base_mhz == 0 {
            return Err(Error::NoBaseClock);
        }
        if caps & CAPS_3V3 == 0 {
            return Err(Error::No3V3);
        }
        let base_hz = base_mhz * 1_000_000;

        self.slow_clock = true;
        self.reset(RESET_ALL)?;
        let present = self.wait(PRESENT_STATE, "the card-detect line to settle", |v| v & CARD_STABLE != 0)?;
        if present & CARD_INSERTED == 0 {
            return Err(Error::NoCard);
        }
        // 3.3 V, the only voltage a card is identified at here: the voltage
        // first, then bus power, then time for the supply to settle (the
        // reset just cut it; Linux allows 10 ms).
        self.write(HOST_CONTROL, POWER_SELECT_3V3);
        self.write(HOST_CONTROL, POWER_SELECT_3V3 | POWER_ON);
        timer::delay_us(10_000);
        // Every status latched, none signalled: this driver polls.
        self.write(INT_STATUS_ENABLE, 0x03ff_00ff);
        self.write(INT_SIGNAL_ENABLE, 0);
        let (ident, ident_hz) = Self::divider(version, base_hz, IDENT_HZ);
        if !(MIN_HZ..=IDENT_HZ).contains(&ident_hz) {
            return Err(Error::ClockRange { base_hz });
        }
        self.set_clock(ident)?;
        let word = self.read(CLOCK_CONTROL) & 0xffff;
        self.write(CLOCK_CONTROL, word | TIMEOUT_MAX);
        // The card needs 74 clocks after power before its first command.
        timer::delay_us(2_000);

        self.command(0, 0, RESP_NONE, 0)?;
        // CMD8: 2.7-3.6 V and the check pattern 0xAA, echoed by a card of
        // spec 2.00 or later; an older card does not answer.
        let v2 = match self.command(8, 0x1aa, R1, 0) {
            Ok(r) if r[0] & 0xfff == 0x1aa => true,
            Ok(r) => return Err(Error::BadEcho(r[0])),
            Err(Error::Command { .. }) => false,
            Err(e) => return Err(e),
        };
        // ACMD41 until the card is out of its busy state, asking for high
        // capacity (HCS) only of a card that answered CMD8.
        let hcs = if v2 { 1 << 30 } else { 0 };
        let deadline = timer::deadline_us(WAIT_MS * 1000);
        let ocr = loop {
            // R3 carries no CRC and no index.
            let r = self.app_command(41, hcs | 0x00ff_8000, RESP_48)?;
            if r[0] & (1 << 31) != 0 {
                break r[0];
            }
            if timer::now_ticks() > deadline {
                return Err(Error::NotReady);
            }
            timer::delay_us(10_000);
        };
        self.high_capacity = ocr & (1 << 30) != 0;

        self.command(2, 0, RESP_136 | CMD_CRC_CHECK, 0)?;
        self.rca = self.command(3, 0, R1, 0)?[0] >> 16;
        let csd = self.command(9, self.rca << 16, RESP_136 | CMD_CRC_CHECK, 0)?;
        self.sectors = csd_sectors(csd)?;
        let r = self.command(7, self.rca << 16, RESP_48_BUSY | CMD_CRC_CHECK | CMD_INDEX_CHECK, 0)?;
        Self::card_status(7, r)?;
        // A 4-bit bus: the card first (ACMD6, argument 2), then the host,
        // and only once the card has said it took it.
        let r = self.app_command(6, 2, R1)?;
        Self::card_status(6, r)?;
        let host = self.read(HOST_CONTROL);
        self.write(HOST_CONTROL, host | HOST_DATA_4BIT);
        if !self.high_capacity {
            let r = self.command(16, SECTOR as u32, R1, 0)?;
            Self::card_status(16, r)?;
            // Byte addresses are 32 bits: no sector past 4 GiB is reachable,
            // whatever the CSD says.
            self.sectors = self.sectors.min(1 << 23);
        }
        let write_protected = self.read(PRESENT_STATE) & WRITE_ENABLED == 0;
        let (data, data_hz) = Self::divider(version, base_hz, DATA_HZ);
        self.set_clock(data)?;
        self.slow_clock = data_hz <= IDENT_HZ;

        console::println!(
            "Ouroboros kernel: sd: SDHCI version field {version}, base clock {base_mhz} MHz; card {} (RCA {:#06x}), {} sectors ({} MiB), identified at {} kHz, 4-bit at {} kHz{}",
            if self.high_capacity { "high capacity, block-addressed" } else { "standard capacity, byte-addressed" },
            self.rca,
            self.sectors,
            self.sectors / 2048,
            ident_hz / 1000,
            data_hz / 1000,
            if write_protected { "; its write-protect switch is set, so it is not written" } else { "" }
        );
        Ok(())
    }

    /// Reads one 512-byte sector: CMD17, then the data port a word at a
    /// time once the controller says the buffer is ready.
    ///
    /// # Safety
    /// As [`Device::init`], which must have succeeded.
    pub unsafe fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> Result<(), Error> {
        let result = self.read_one(sector, buf);
        if let Err(Error::Timeout(_) | Error::CardStatus { .. }) = result {
            // A wait that ran out, or a card status reporting an error after
            // the command was taken, left the lines as they were, a block
            // perhaps still in flight: reset both, so the next command starts
            // clean rather than waiting on an inhibit nothing will lift. A
            // controller-reported error was reset in `failed`.
            self.abandon();
        }
        result
    }

    /// The command and data lines reset and every status cleared.
    fn abandon(&self) {
        let _ = self.reset(RESET_CMD | RESET_DAT);
        self.write(INT_STATUS, 0xffff_ffff);
    }

    /// Waits for the command and data lines to be free: the controller
    /// ignores a block size or count written while a transfer is in flight.
    fn lines_free(&self) -> Result<(), Error> {
        self.wait(PRESENT_STATE, "the command and data lines to be free", |v| v & (CMD_INHIBIT | DAT_INHIBIT) == 0)
            .map(|_| ())
    }

    fn read_one(&mut self, sector: u64, buf: &mut [u8; 512]) -> Result<(), Error> {
        let address = self.address(sector)?;
        self.lines_free()?;
        self.write(BLOCK_SIZE_COUNT, (1 << 16) | SECTOR as u32);
        let r = self.command(
            17,
            address,
            RESP_48 | CMD_CRC_CHECK | CMD_INDEX_CHECK | CMD_DATA_PRESENT,
            XFER_READ | XFER_BLOCK_COUNT_ENABLE,
        )?;
        Self::card_status(17, r)?;
        let status =
            self.wait(INT_STATUS, "the read buffer", |v| v & (INT_BUFFER_READ_READY | INT_ERROR) != 0)?;
        if status & INT_ERROR != 0 {
            return Err(self.failed(17, status));
        }
        self.write(INT_STATUS, INT_BUFFER_READ_READY);
        for word in buf.chunks_exact_mut(4) {
            word.copy_from_slice(&self.read(BUFFER_DATA).to_le_bytes());
        }
        self.wait_transfer(17)
    }

    /// Writes one 512-byte sector: CMD24, then the data port a word at a
    /// time once the controller says the buffer can take them, then the
    /// transfer's end, which the controller signals once the card has left
    /// its busy state. Refused inside the firmware's partition, before
    /// anything is sent.
    ///
    /// # Safety
    /// As [`Device::read_sector`].
    pub unsafe fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> Result<(), Error> {
        if let Some((first, count)) = self.protecting(sector, 1) {
            self.refused += 1;
            if self.refused == 1 {
                console::println!(
                    "Ouroboros kernel: sd: a write to sector {sector} refused: sectors {first} to {} hold the card's firmware partition or the table that finds it, never written",
                    first + count - 1
                );
            }
            return Err(Error::Protected(sector));
        }
        let result = self.write_one(sector, buf);
        if let Err(Error::Timeout(_) | Error::CardStatus { .. }) = result {
            self.abandon();
        }
        result
    }

    fn write_one(&mut self, sector: u64, buf: &[u8; 512]) -> Result<(), Error> {
        let address = self.address(sector)?;
        if self.read(PRESENT_STATE) & WRITE_ENABLED == 0 {
            return Err(Error::WriteProtected);
        }
        self.lines_free()?;
        self.write(BLOCK_SIZE_COUNT, (1 << 16) | SECTOR as u32);
        let r = self.command(
            24,
            address,
            RESP_48 | CMD_CRC_CHECK | CMD_INDEX_CHECK | CMD_DATA_PRESENT,
            XFER_BLOCK_COUNT_ENABLE,
        )?;
        Self::card_status(24, r)?;
        let status =
            self.wait(INT_STATUS, "the write buffer", |v| v & (INT_BUFFER_WRITE_READY | INT_ERROR) != 0)?;
        if status & INT_ERROR != 0 {
            return Err(self.failed(24, status));
        }
        self.write(INT_STATUS, INT_BUFFER_WRITE_READY);
        for word in buf.chunks_exact(4) {
            self.write(BUFFER_DATA, u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
        }
        self.wait_transfer(24)
    }
}

/// The card's size in 512-byte sectors from its CSD, as the controller
/// returns it: the 136-bit response without its CRC byte, so CSD bit `n` is
/// bit `n - 8` of the four words.
fn csd_sectors(r: [u32; 4]) -> Result<u64, Error> {
    let csd = (u128::from(r[0]) | u128::from(r[1]) << 32 | u128::from(r[2]) << 64 | u128::from(r[3]) << 96) << 8;
    let bits = |hi: u32, lo: u32| ((csd >> lo) & ((1u128 << (hi - lo + 1)) - 1)) as u64;
    match bits(127, 126) {
        // CSD 1.0: (C_SIZE + 1) * 2^(C_SIZE_MULT + 2) blocks of 2^READ_BL_LEN bytes.
        0 => {
            let bytes = (bits(73, 62) + 1) << (bits(49, 47) + 2) << bits(83, 80);
            Ok(bytes / SECTOR as u64)
        }
        // CSD 2.0: (C_SIZE + 1) * 512 KiB.
        1 => Ok((bits(69, 48) + 1) * 1024),
        v => Err(Error::UnknownCsd(v as u8)),
    }
}
