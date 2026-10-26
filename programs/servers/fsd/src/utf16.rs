//! Long names as UTF-16 code units, the form exFAT and FAT32's long names
//! both store: how a listing shows them as UTF-8, and when it cannot. The
//! one copy both arms render through, moved here from `exfat.rs` on
//! 2026-10-26 when FAT32 stopped rendering a name a byte a unit, `?` for
//! every unit outside ASCII, so `café` and `cafè` were one name there as
//! they had been on exFAT until 2026-10-21.

/// The most bytes a listed name takes: 255, as every other arm's names (ext2's
/// are bytes), so a name always fits one `NP_READDIR` page (512 bytes) and a
/// path a lookup takes. A name whose UTF-8 is longer is listed ASCII (see
/// [`shown_exactly`]).
pub(crate) const NAME_SHOWN_MAX: usize = 255;

/// The code point at `units[i]` and the units it takes, or `None` for an
/// unpaired surrogate.
pub(crate) fn code_point(units: &[u16], i: usize) -> Option<(u32, usize)> {
    let u = units[i] as u32;
    match u {
        0xd800..=0xdbff if i + 1 < units.len() && (0xdc00..=0xdfff).contains(&(units[i + 1] as u32)) => {
            Some((0x10000 + ((u - 0xd800) << 10) + (units[i + 1] as u32 - 0xdc00), 2))
        }
        0xd800..=0xdfff => None,
        _ => Some((u, 1)),
    }
}

/// Whether a listing shows `units` as their UTF-8: they hold no unpaired
/// surrogate and that UTF-8 is at most [`NAME_SHOWN_MAX`] bytes. A name that is
/// not is shown ASCII, `?` for each unit outside it, and a lookup's `?` stands
/// for such a unit (exFAT's `name_answers`; FAT32's lookup compares this rendering), so the name is still reached by what
/// is listed. A create cannot make that name, since `?` is not allowed in
/// one, so it is never listed twice; two such names that differ only outside
/// ASCII answer to one spelling, the first (the roadmap). Linux lists both
/// in full; a page here is 512 bytes.
pub(crate) fn shown_exactly(units: &[u16]) -> bool {
    let mut bytes = 0usize;
    let mut i = 0usize;
    while i < units.len() {
        let Some((c, n)) = code_point(units, i) else {
            return false;
        };
        bytes += match c {
            0..=0x7f => 1,
            0x80..=0x7ff => 2,
            0x800..=0xffff => 3,
            _ => 4,
        };
        i += n;
    }
    bytes <= NAME_SHOWN_MAX
}

/// A name's UTF-16 `units` as a listing shows them, in `out`: their UTF-8 when
/// [`shown_exactly`], otherwise ASCII with `?` for each unit outside it, at
/// most [`NAME_SHOWN_MAX`] bytes either way. Encoded by hand, without
/// `char::encode_utf8`, whose panic formats (the PIE link's trap).
pub(crate) fn utf8_name<'a>(units: &[u16], out: &'a mut [u8; NAME_SHOWN_MAX]) -> &'a str {
    let mut n = 0usize;
    if !shown_exactly(units) {
        for &u in units {
            out[n] = if u < 0x80 { u as u8 } else { b'?' };
            n += 1;
        }
        return core::str::from_utf8(&out[..n]).unwrap_or("?");
    }
    let mut i = 0usize;
    while i < units.len() {
        let Some((c, used)) = code_point(units, i) else {
            break; // not reached: shown_exactly found none unpaired
        };
        i += used;
        if c < 0x80 {
            out[n] = c as u8;
            n += 1;
        } else if c < 0x800 {
            out[n] = 0xc0 | (c >> 6) as u8;
            out[n + 1] = 0x80 | (c & 0x3f) as u8;
            n += 2;
        } else if c < 0x10000 {
            out[n] = 0xe0 | (c >> 12) as u8;
            out[n + 1] = 0x80 | ((c >> 6) & 0x3f) as u8;
            out[n + 2] = 0x80 | (c & 0x3f) as u8;
            n += 3;
        } else {
            out[n] = 0xf0 | (c >> 18) as u8;
            out[n + 1] = 0x80 | ((c >> 12) & 0x3f) as u8;
            out[n + 2] = 0x80 | ((c >> 6) & 0x3f) as u8;
            out[n + 3] = 0x80 | (c & 0x3f) as u8;
            n += 4;
        }
    }
    // Valid by construction; `?` would show an encoder bug, not hide it.
    core::str::from_utf8(&out[..n]).unwrap_or("?")
}
