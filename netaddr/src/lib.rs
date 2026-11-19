//! An IPv4 address and a port as text: the one parser of a dotted address
//! ([`parse_ip`]), its one formatter ([`format_ip`]), and the one decimal
//! reader `netd` takes a port or a connection number with ([`parse_dec`]).
//! Pure, no dependencies, so the rules run on the host (`make test`) and
//! every caller, kernel-free or not, can link it.
//!
//! **Why a crate of its own** (2026-11-19, the review of #490): five
//! parsers of a dotted address had four sets of rules. `netconf`'s and
//! `clusterkeys`' refused a part of more than three digits; the shell's and
//! `ping`'s took any number of leading zeros; and `netd`'s took any digits
//! and wrapped, so `18446744073709551617.0.0.1` was `1.0.0.1`, as its
//! decimal reader made a `connect` port of 18446744073709551696 port 80.
//! The first version of the change put the one parser in `netconf`, which
//! is `/etc/net`'s, and so tied the cluster's identity files, the shell
//! and `ping` to `netd`'s configuration; this crate is below all of them.
//!
//! Callers: `netconf` (`/etc/net`), `clusterkeys` (`/etc/cluster/authorized`)
//! and the `clusterkey` tool, `netd` (`/net/tcp` `connect` and `announce`,
//! `/net/ip`), the shell (`mount -r`, `cpu`, the mount listing) and `ping`.
//! A change here owes the rigs `docs/testing/make-targets.md` names for it.

#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// Four decimal numbers of 0 to 255 with single dots between, and nothing
/// else: no sign, no space, no empty part, at most three digits a part
/// (`010.000.002.015` is `10.0.2.15`; `0010.0.2.15` is refused).
pub fn parse_ip(word: &[u8]) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut part = 0;
    let mut digits = 0;
    let mut value = 0u32;
    for &b in word {
        match b {
            b'0'..=b'9' => {
                digits += 1;
                value = value * 10 + u32::from(b - b'0');
                if digits > 3 || value > 255 {
                    return None;
                }
            }
            b'.' if digits > 0 && part < 3 => {
                ip[part] = value as u8;
                part += 1;
                digits = 0;
                value = 0;
            }
            _ => return None,
        }
    }
    if part != 3 || digits == 0 {
        return None;
    }
    ip[3] = value as u8;
    Some(ip)
}

/// The longest address [`format_ip`] writes: `255.255.255.255`.
pub const IP_TEXT_MAX: usize = 15;

/// `ip` as a dotted address into `out`, no leading zeros; returns its
/// length. [`parse_ip`] reads it back as `ip`.
pub fn format_ip(ip: &[u8; 4], out: &mut [u8; IP_TEXT_MAX]) -> usize {
    let mut n = 0;
    for (i, &v) in ip.iter().enumerate() {
        if i > 0 {
            out[n] = b'.';
            n += 1;
        }
        if v >= 100 {
            out[n] = b'0' + v / 100;
            n += 1;
        }
        if v >= 10 {
            out[n] = b'0' + (v / 10) % 10;
            n += 1;
        }
        out[n] = b'0' + v % 10;
        n += 1;
    }
    n
}

/// Decimal digits, and nothing else, as a `u64`. `None` on an empty word,
/// a byte that is not a digit (no sign, no space), or a value past
/// `u64::MAX`, which is refused and never wrapped. A caller with a smaller
/// bound (a port's 65535) checks it on the value.
pub fn parse_dec(word: &[u8]) -> Option<u64> {
    if word.is_empty() {
        return None;
    }
    let mut v = 0u64;
    for &d in word {
        if !d.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
    }
    Some(v)
}
