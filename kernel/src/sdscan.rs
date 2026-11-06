//! What the firmware says of the SD card's controller: step 0 of
//! `docs/roadmap/roadmap-sd-card.md`, read before the exit and logged
//! through the UEFI console. It touches no controller register and drives
//! nothing; it reports the three sources a driver could find the
//! controller by, so the driver's discovery is written against what a board
//! shows and not against the firmware's sources alone.
//!
//! - **The devicetree**, a node `compatible = "brcm,bcm2711-emmc2"`, its
//!   `reg` translated to a CPU address (`dtranges.rs`). The Pi 4's firmware
//!   in its ACPI mode publishes no devicetree at all (`NoDtb`, seen
//!   2026-11-05), so this line is expected to say so there.
//! - **The ACPI tables**, any DSDT or SSDT holding `Name (_HID, "BRCME88C")`
//!   (EMMC2, edk2-platforms `Emmc.asl`, an SSDT of its own) or
//!   `Name (_HID, "BCM2847")` (the Arasan controller, `Sdhc.asl`). Found by
//!   a byte search, which is enough for a report and is not a declaration:
//!   the device's registers are set inside a `_CRS` method there, which
//!   `amlscan` does not evaluate. With the boot flag `\SDDUMP`, each table
//!   holding one is printed in hex, so that `_CRS` can be read and the
//!   driver's parser written and host-tested against the real bytes.
//! - **The firmware's routing setting**, the UEFI variable `SdIsArasan`
//!   (edk2-platforms `ConfigDxe`): 0 routes the card's pins to EMMC2, 1 to
//!   Arasan. Found by name among the variables, so its vendor GUID is read
//!   off the board rather than copied from a header.

use crate::acpi;
use crate::bootflags;
use fdt::Fdt;
use uefi::{cstr16, CStr16};

/// The `_HID`s searched for, with what each is.
const HIDS: [(&[u8], &str); 2] = [(b"BRCME88C", "EMMC2"), (b"BCM2847", "Arasan")];

/// The firmware variable that routes the card (edk2-platforms `ConfigDxe`).
const ROUTING_VAR: &CStr16 = cstr16!("SdIsArasan");

/// Logs what the devicetree, the ACPI tables and the firmware's variables
/// say of the SD controller; with `flags.sd_dump`, prints each ACPI table
/// that declares one. Before `exit_boot_services` only (the UEFI console,
/// runtime variables and the tables' mapping are all relied on).
///
/// # Safety
/// `dtb` and `rsdp` as `devicetree::find_dtb` and `acpi::find_rsdp` return
/// them: each, if `Some`, points at its firmware structure, mapped.
pub unsafe fn report(dtb: Option<*const u8>, rsdp: Option<*const u8>, flags: &bootflags::Flags) {
    unsafe { report_devicetree(dtb) };
    unsafe { report_acpi(rsdp, flags.sd_dump) };
    report_routing();
}

unsafe fn report_devicetree(dtb: Option<*const u8>) {
    let Some(dtb) = dtb else {
        log::info!("Ouroboros kernel: sd: no devicetree, so no emmc2 node");
        return;
    };
    let Ok(fdt) = (unsafe { Fdt::from_ptr(dtb) }) else {
        log::info!("Ouroboros kernel: sd: the devicetree does not parse");
        return;
    };
    let Some(node) = fdt.find_compatible(&["brcm,bcm2711-emmc2"]) else {
        log::info!("Ouroboros kernel: sd: the devicetree has no brcm,bcm2711-emmc2 node");
        return;
    };
    match node.reg().and_then(|mut regs| regs.next()) {
        Some(region) => {
            let bus = region.starting_address as u64;
            let size = region.size.unwrap_or(0);
            match crate::dtranges::to_cpu(&fdt, node, bus) {
                Some(cpu) => log::info!(
                    "Ouroboros kernel: sd: devicetree emmc2 {}: bus {bus:#x}, CPU {cpu:#x}, size {size:#x}",
                    node.name
                ),
                None => log::info!(
                    "Ouroboros kernel: sd: devicetree emmc2 {}: bus {bus:#x}, size {size:#x}, no CPU address (ranges)",
                    node.name
                ),
            }
        }
        None => log::info!("Ouroboros kernel: sd: devicetree emmc2 {} has no reg", node.name),
    }
}

unsafe fn report_acpi(rsdp: Option<*const u8>, dump: bool) {
    let mut tables: [&[u8]; amlscan::MAX_TABLES] = [&[]; amlscan::MAX_TABLES];
    let count = match unsafe { acpi::aml_table_slices(rsdp, &mut tables) } {
        Ok(count) => count,
        Err(e) => {
            log::info!("Ouroboros kernel: sd: ACPI tables not read ({e:?})");
            return;
        }
    };
    let mut any = false;
    for table in &tables[..count] {
        // The header's names as text a console takes: the Pi's DSDT's OEM
        // table ID holds bytes the UEFI console refuses (seen 2026-11-05: the
        // line stopped at the ID, and the dump's first line panicked).
        let (sig_buf, id_buf) = (printable::<4>(&table[0..4]), printable::<8>(&table[16..24]));
        let signature = core::str::from_utf8(&sig_buf).unwrap_or("????");
        let oem_table = core::str::from_utf8(&id_buf).unwrap_or("????????");
        let mut declares = false;
        for (hid, what) in HIDS {
            if let Some(at) = amlscan::hid_string_at(table, hid) {
                declares = true;
                any = true;
                log::info!(
                    "Ouroboros kernel: sd: {signature} {oem_table} ({:#x} bytes) holds Name (_HID, \"{}\") at {at:#x}: {what}",
                    table.len(),
                    core::str::from_utf8(hid).unwrap_or("?")
                );
            }
        }
        if declares && dump {
            dump_table(signature, oem_table, table);
        }
    }
    if !any {
        log::info!("Ouroboros kernel: sd: no ACPI table ({count} read) names an SD controller's _HID");
    } else if !dump {
        log::info!("Ouroboros kernel: sd: touch SDDUMP at the ESP's root to print those tables");
    }
}

/// `bytes` with every byte that is not printable ASCII as `.`, so a table's
/// header names reach the console whatever the firmware wrote in them.
fn printable<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [b'.'; N];
    for (o, &b) in out.iter_mut().zip(bytes) {
        if b.is_ascii_graphic() || b == b' ' {
            *o = b;
        }
    }
    out
}

/// One line on the UEFI console, an error dropped rather than a panic
/// (`uefi::println!` panics on one): a diagnostic must not end the boot.
fn out_line(args: core::fmt::Arguments) {
    uefi::system::with_stdout(|out| {
        let _ = core::fmt::Write::write_fmt(out, args);
        let _ = core::fmt::Write::write_str(out, "\r\n");
    });
}

/// A table in hex, 16 bytes a line, each line its own and short enough for
/// an 80-column console (the Pi's serial log wraps the logger's longer
/// lines), with no logger prefix to wrap it.
fn dump_table(signature: &str, oem_table: &str, table: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out_line(format_args!("SDDUMP {signature} {oem_table} begin {:#x}", table.len()));
    for (line, chunk) in table.chunks(16).enumerate() {
        let mut text = [b' '; 32];
        for (i, &b) in chunk.iter().enumerate() {
            text[2 * i] = HEX[usize::from(b >> 4)];
            text[2 * i + 1] = HEX[usize::from(b & 0xf)];
        }
        let hex = core::str::from_utf8(&text[..2 * chunk.len()]).unwrap_or("");
        out_line(format_args!("SDDUMP {:05x} {hex}", line * 16));
    }
    out_line(format_args!("SDDUMP {signature} {oem_table} end"));
}

fn report_routing() {
    for key in uefi::runtime::variable_keys() {
        let key = match key {
            Ok(key) => key,
            Err(e) => {
                log::info!("Ouroboros kernel: sd: variables not listed ({:?}), {ROUTING_VAR} unread", e.status());
                return;
            }
        };
        if &*key.name != ROUTING_VAR {
            continue;
        }
        match uefi::runtime::get_variable_boxed(&key.name, &key.vendor) {
            Ok((value, _)) => {
                let mut word = [0u8; 4];
                let n = value.len().min(4);
                word[..n].copy_from_slice(&value[..n]);
                let v = u32::from_le_bytes(word);
                let to = match v {
                    0 => "EMMC2",
                    1 => "Arasan",
                    _ => "unknown",
                };
                log::info!(
                    "Ouroboros kernel: sd: {ROUTING_VAR} = {v} ({} bytes, vendor {}): the card is routed to {to}",
                    value.len(),
                    key.vendor.0
                );
            }
            Err(e) => log::info!("Ouroboros kernel: sd: {ROUTING_VAR} found but not read ({:?})", e.status()),
        }
        return;
    }
    log::info!("Ouroboros kernel: sd: no {ROUTING_VAR} variable: the firmware's default routing applies");
}
